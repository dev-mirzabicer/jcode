/// Claude Code OAuth beta headers used by the Anthropic transport.
pub const ANTHROPIC_OAUTH_BETA_HEADERS: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,effort-2025-11-24";

/// Claude Code OAuth beta headers with Anthropic's explicit 1M context beta.
pub const ANTHROPIC_OAUTH_BETA_HEADERS_1M: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,effort-2025-11-24,context-1m-2025-08-07";

/// How a Claude model exposes its 1M-token long-context window.
///
/// These classifications were verified against the live Anthropic API on a
/// Claude subscription (raw 250K-token requests): the catalog's
/// `max_input_tokens` field is not a reliable signal because it over-advertises
/// 1M for models that are still hard-capped at 200K (e.g. `claude-sonnet-4-5`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicContextMode {
    /// 1M input window available by default, no beta header or `[1m]` opt-in
    /// needed (e.g. `claude-opus-4-8`, `claude-opus-4-7`).
    Native1M,
    /// 200K by default; 1M available as an opt-in via the `context-1m` beta
    /// header (the `[1m]` suffix), which may require usage credits
    /// (e.g. `claude-opus-4-6`, `claude-sonnet-4-6`).
    OptIn1M,
    /// 200K input window, with no 1M path (e.g. `claude-opus-4-5`,
    /// `claude-sonnet-4-5`, `claude-haiku-4-5`).
    Standard,
}

impl AnthropicContextMode {
    /// The default context window (in tokens) for this mode, i.e. what a request
    /// gets without opting in to the 1M beta.
    pub fn default_context_window(self) -> usize {
        match self {
            AnthropicContextMode::Native1M => 1_000_000,
            AnthropicContextMode::OptIn1M | AnthropicContextMode::Standard => 200_000,
        }
    }

    /// The context window (in tokens) when the 1M long-context path is engaged
    /// (the `[1m]` suffix). For `Standard` models there is no 1M path, so this is
    /// the same as the default.
    pub fn long_context_window(self) -> usize {
        match self {
            AnthropicContextMode::Native1M => 1_000_000,
            // Anthropic's opt-in beta advertises a 1,048,576-token window.
            AnthropicContextMode::OptIn1M => 1_048_576,
            AnthropicContextMode::Standard => 200_000,
        }
    }

    /// Whether this model has any 1M long-context path at all (native or opt-in).
    pub fn has_1m_window(self) -> bool {
        !matches!(self, AnthropicContextMode::Standard)
    }

    /// Whether jcode should surface a distinct `[1m]` picker alias for this model.
    /// Only opt-in models benefit, native-1M models already use 1M by default so
    /// a `[1m]` alias would be a redundant duplicate.
    pub fn exposes_1m_alias(self) -> bool {
        matches!(self, AnthropicContextMode::OptIn1M)
    }
}

/// Classify how a Claude model exposes long context. Accepts both canonical
/// (`claude-opus-4-8`) and dotted (`claude-opus-4.8`) forms, with or without a
/// trailing `[1m]` suffix.
///
/// Known generations are pinned to behavior verified against the live API.
/// Unknown *future* generations are classified by parsed family/version rather
/// than a hardcoded prefix list, and default optimistically to `Native1M` from
/// version 5 on. Failing closed at 200K is the worse error: it silently
/// under-reports the context meter and shrinks compaction budgets ~5x with no
/// diagnostic (issues #450, #577, #578).
pub fn anthropic_context_mode(model: &str) -> AnthropicContextMode {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return AnthropicContextMode::Standard;
    }
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return AnthropicContextMode::Standard;
    };

    match family {
        // Opus/Sonnet: 4.7+ and 5+ are native 1M; 4.6 opts in via the
        // context-1m beta; 4.5 and older are hard-capped at 200K.
        Some("opus") | Some("sonnet") => {
            if version >= (4, 7) {
                AnthropicContextMode::Native1M
            } else if version == (4, 6) {
                AnthropicContextMode::OptIn1M
            } else {
                AnthropicContextMode::Standard
            }
        }
        // Haiku 4.5 is 200K. Newer small models are covered by the
        // version-5 optimistic default below.
        Some("haiku") if version < (5, 0) => AnthropicContextMode::Standard,
        // Optimistic default for new generations (Fable 5, Haiku 5, future
        // families): assume native 1M from version 5 on.
        _ => {
            if version >= (5, 0) {
                AnthropicContextMode::Native1M
            } else {
                AnthropicContextMode::Standard
            }
        }
    }
}

/// Check if a model name explicitly requests 1M context via suffix
/// (for example `claude-opus-4-6[1m]`).
pub fn anthropic_is_1m_model(model: &str) -> bool {
    model.ends_with("[1m]")
}

/// Whether `model` looks like a Claude id with a parseable family/version, i.e.
/// one [`anthropic_context_mode`] can classify rather than guess about.
pub fn claude_id_has_parseable_version(model: &str) -> bool {
    let base = normalized_claude_caps_key(model);
    base.starts_with("claude") && parse_claude_family_version(&base).1.is_some()
}

/// Whether [`anthropic_context_mode`]'s answer for `model` comes from a
/// generation whose long-context behavior was verified against the live
/// Anthropic API, as opposed to the optimistic default for new generations.
///
/// Callers use this to decide precedence: a verified classification beats the
/// live catalog (whose `max_input_tokens` over-advertises 1M for 200K-capped
/// models), while an unverified one should yield to catalog/config data and be
/// used only as a last resort instead of the 200K default.
pub fn anthropic_context_mode_is_verified(model: &str) -> bool {
    let base = normalized_claude_caps_key(model);
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return false;
    };
    match family {
        // Opus/Sonnet 3.x-4.8 and Sonnet 5 were probed with raw long-context
        // requests on a live subscription.
        Some("opus") => version <= (4, 8),
        Some("sonnet") => version <= (5, 0),
        Some("haiku") => version <= (4, 5),
        _ => false,
    }
}

/// Maximum output tokens Anthropic's synchronous Messages API accepts for a
/// model, per the published model comparison table.
///
/// This matters more than it looks. Adaptive-thinking models spend their output
/// budget on thinking *and* the visible tool call, so a budget that is too
/// small truncates mid-tool-call and silently ends an agent turn. jcode used a
/// flat 32K default for every Claude model, which cut long agentic turns on
/// models that actually allow 128K.
pub fn anthropic_max_output_tokens(model: &str) -> u32 {
    let base = anthropic_strip_1m_suffix(model.trim()).to_ascii_lowercase();

    // Opus 5, Opus 4.6-4.8, Sonnet 5, Sonnet 4.6, and Fable/Mythos 5 all
    // advertise 128K max output on the synchronous Messages API.
    const LARGE_OUTPUT_PREFIXES: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4.8",
        "claude-opus-4-7",
        "claude-opus-4.7",
        "claude-opus-4-6",
        "claude-opus-4.6",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4.6",
        "claude-fable-5",
        "claude-fable",
        "claude-mythos",
    ];
    if LARGE_OUTPUT_PREFIXES
        .iter()
        .any(|prefix| base.starts_with(prefix))
    {
        return 128_000;
    }

    // Haiku 4.5 tops out at 64K.
    if base.starts_with("claude-haiku-4-5") || base.starts_with("claude-haiku-4.5") {
        return 64_000;
    }

    // Older/unknown generations keep the conservative 32K jcode has always used.
    32_768
}

/// Check if a model explicitly requests 1M context via the `[1m]` suffix.
pub fn anthropic_effectively_1m(model: &str) -> bool {
    anthropic_is_1m_model(model)
}

/// Strip the `[1m]` suffix to get the actual API model name.
pub fn anthropic_strip_1m_suffix(model: &str) -> &str {
    crate::model_id::strip_long_context_suffix(model)
}

/// Get the OAuth beta header value appropriate for the model.
pub fn anthropic_oauth_beta_headers(model: &str) -> &'static str {
    if anthropic_is_1m_model(model) {
        ANTHROPIC_OAUTH_BETA_HEADERS_1M
    } else {
        ANTHROPIC_OAUTH_BETA_HEADERS
    }
}

/// How a Claude model exposes reasoning effort and thinking on the live
/// Messages API.
///
/// This is the single source of truth shared by the Anthropic runtime (request
/// building, `set_reasoning_effort` validation) and the TUI effort cycler, so
/// new models cannot drift between the two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnthropicReasoningCaps {
    /// Accepts `output_config: {effort}`.
    pub output_effort: bool,
    /// Accepts `thinking: {type: adaptive}`.
    pub adaptive_thinking: bool,
    /// Needs `thinking: {type: enabled, budget_tokens}` (manual budgets).
    pub manual_thinking: bool,
    /// Accepts the `xhigh` effort level.
    pub xhigh_effort: bool,
    /// Accepts the `max` effort level.
    pub max_effort: bool,
    /// Whether a thinking block is invalidated by an edit to the conversation
    /// before it (INT-01 D11).
    pub reasoning_binding: ReasoningBinding,
    /// How a request asks the model not to think (effort `none`).
    pub thinking_off: ThinkingOff,
    /// Whether the model accepts sampling parameters such as `temperature`.
    pub sampling_parameters: bool,
}

/// How a request asks a Claude model not to think, the meaning of jcode's
/// effort `none` (INT-01 DESIGN §7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThinkingOff {
    /// Omitting `thinking` runs the model without thinking.
    #[default]
    OmitThinking,
    /// Omitting `thinking` runs adaptive thinking; `thinking: {type:
    /// "disabled"}` turns it off.
    Disabled,
    /// Thinking cannot be turned off. The lowest setting is effort `low`, which
    /// is what `none` means for this model.
    AlwaysOn,
}

/// Whether Anthropic binds a model's signed thinking to the conversation that
/// produced it ("preserved thinking").
///
/// This is policy as data (INT-01 D11): every jcode mechanism that exists only
/// because of binding reads this value and does nothing for `Unbound`. When
/// Anthropic changes a model's behavior, the change is one evidence-cited
/// entry in [`anthropic_reasoning_binding`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningBinding {
    /// Editing the system prompt, the tool set or any earlier message
    /// invalidates every later thinking block.
    PrefixBound,
    /// Edits before a block do not invalidate it.
    #[default]
    Unbound,
}

impl AnthropicReasoningCaps {
    /// Full modern ladder: `output_config` effort low..xhigh/max + adaptive thinking.
    const FULL: Self = Self {
        output_effort: true,
        adaptive_thinking: true,
        manual_thinking: false,
        xhigh_effort: true,
        max_effort: true,
        reasoning_binding: ReasoningBinding::Unbound,
        thinking_off: ThinkingOff::OmitThinking,
        sampling_parameters: true,
    };
    /// `output_config` effort + adaptive thinking, but no `xhigh` level.
    const EFFORT_NO_XHIGH: Self = Self {
        output_effort: true,
        adaptive_thinking: true,
        manual_thinking: false,
        xhigh_effort: false,
        max_effort: true,
        reasoning_binding: ReasoningBinding::Unbound,
        thinking_off: ThinkingOff::OmitThinking,
        sampling_parameters: true,
    };
    /// `output_config` effort with manual thinking budgets (Opus 4.5).
    const MANUAL_WITH_EFFORT: Self = Self {
        output_effort: true,
        adaptive_thinking: false,
        manual_thinking: true,
        xhigh_effort: false,
        max_effort: false,
        reasoning_binding: ReasoningBinding::Unbound,
        thinking_off: ThinkingOff::OmitThinking,
        sampling_parameters: true,
    };
    /// Manual thinking budgets only (Claude 3.7 Sonnet).
    const MANUAL_ONLY: Self = Self {
        output_effort: false,
        adaptive_thinking: false,
        manual_thinking: true,
        xhigh_effort: false,
        max_effort: false,
        reasoning_binding: ReasoningBinding::Unbound,
        thinking_off: ThinkingOff::OmitThinking,
        sampling_parameters: true,
    };
    const NONE: Self = Self {
        output_effort: false,
        adaptive_thinking: false,
        manual_thinking: false,
        xhigh_effort: false,
        max_effort: false,
        reasoning_binding: ReasoningBinding::Unbound,
        thinking_off: ThinkingOff::OmitThinking,
        sampling_parameters: true,
    };

    /// Whether any reasoning-effort control is available at all.
    pub fn supports_reasoning_effort(self) -> bool {
        self.output_effort || self.manual_thinking
    }
}

/// Normalize a Claude id for capability matching: lowercase, `[1m]` and
/// `-YYYYMMDD` date suffixes stripped, dotted versions (`4.6`) dashed (`4-6`).
fn normalized_claude_caps_key(model: &str) -> String {
    let base = anthropic_strip_1m_suffix(model.trim())
        .to_ascii_lowercase()
        .replace('.', "-");
    crate::model_id::strip_date_suffix(&base).to_string()
}

/// Parse `(family, version)` from a normalized Claude id. Handles both
/// version-last (`claude-sonnet-4-6`) and version-first (`claude-3-7-sonnet`)
/// forms. A single version number means `.0` (`claude-sonnet-5` -> 5.0).
fn parse_claude_family_version(base: &str) -> (Option<&str>, Option<(u32, u32)>) {
    let mut family = None;
    let mut nums: Vec<u32> = Vec::new();
    for segment in base.split('-') {
        if segment == "claude" {
            continue;
        }
        if let Ok(num) = segment.parse::<u32>() {
            if nums.len() < 2 {
                nums.push(num);
            }
        } else if family.is_none() && segment.chars().all(|c| c.is_ascii_alphabetic()) {
            family = Some(segment);
        }
    }
    let version = match nums.as_slice() {
        [] => None,
        [major] => Some((*major, 0)),
        [major, minor, ..] => Some((*major, *minor)),
    };
    (family, version)
}

/// Reasoning-effort capabilities for a Claude model.
///
/// Known generations are pinned to what the live API accepts (verified live
/// 2026-07-01 for Fable 5 / Opus 4.x, 2026-07-07 for Sonnet 5). Unknown
/// *future* generations (version 5+ in any family) optimistically default to
/// the full ladder: the Anthropic runtime self-heals by stripping the
/// reasoning fields and retrying if a model rejects them, so optimism degrades
/// gracefully while pessimism silently disables effort until someone probes
/// the model and updates a table.
pub fn anthropic_reasoning_caps(model: &str) -> AnthropicReasoningCaps {
    AnthropicReasoningCaps {
        reasoning_binding: anthropic_reasoning_binding(model),
        thinking_off: anthropic_thinking_off(model),
        sampling_parameters: anthropic_accepts_sampling_parameters(model),
        ..anthropic_effort_caps(model)
    }
}

/// How a Claude model's request turns thinking off.
///
/// Entries follow Anthropic's model documentation (claude-api reference,
/// re-verified 2026-10-01): omitting `thinking` runs Opus 4.8/4.7 and every
/// earlier generation without thinking; Opus 5 and Sonnet 5 run adaptive
/// thinking when it is omitted and accept `{type: "disabled"}` (Opus 5 only at
/// effort `high` or below, which `none` satisfies because it sends no effort);
/// Opus 5.5, Sonnet 5.5, Fable and Mythos reject `disabled`. Sonnet 5.5's
/// `{type: "between_tools"}` is not used: it rejects `block_binding`, which
/// would remove the binding safety net from a prefix-bound model. An unknown
/// future generation is `AlwaysOn`, which never sends a rejected shape.
pub fn anthropic_thinking_off(model: &str) -> ThinkingOff {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return ThinkingOff::OmitThinking;
    }
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return ThinkingOff::OmitThinking;
    };
    if version < (5, 0) {
        return ThinkingOff::OmitThinking;
    }
    match family {
        Some("opus") | Some("sonnet") if version == (5, 0) => ThinkingOff::Disabled,
        _ => ThinkingOff::AlwaysOn,
    }
}

/// Whether a Claude model accepts sampling parameters (`temperature`).
///
/// Anthropic's documentation removes them on Opus 4.7 and later, Sonnet 5 and
/// later, Fable and Mythos (Sonnet 5.5 rejects non-default values). INT-01
/// Gate 0 (G0.5, 2026-09-29, Claude OAuth) measured `temperature: 1.0`
/// accepted on Opus 5.5, Opus 5, Sonnet 5.5 and Sonnet 5, so omitting it there
/// is hygiene rather than the fix for an observed rejection. Unknown future
/// generations follow the documented direction and receive none.
pub fn anthropic_accepts_sampling_parameters(model: &str) -> bool {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return true;
    }
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return true;
    };
    match family {
        Some("opus") => version < (4, 7),
        Some("sonnet") | Some("haiku") => version < (5, 0),
        _ => version < (5, 0),
    }
}

/// jcode's reasoning effort for a Claude model when none is configured, or
/// `None` to leave the model's own default (INT-01 DESIGN §7).
///
/// An explicit table, not name matching: Opus 5.5 gets `medium`, its API
/// default; Opus 5 `low`, enough for day-to-day agentic work; Opus 4.7/4.8
/// `xhigh`, Anthropic's recommended start for coding; earlier Opus with an
/// effort control `high`; Fable `high`. Sonnet, Haiku, Mythos and unknown
/// generations keep the model default, so a new generation never inherits a
/// neighbour's policy by accident.
pub fn anthropic_default_reasoning_effort(model: &str) -> Option<&'static str> {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return None;
    }
    let (family, version) = parse_claude_family_version(&base);
    let version = version?;
    match family {
        Some("opus") => match version {
            (5, 5) => Some("medium"),
            (5, 0) => Some("low"),
            (4, 7) | (4, 8) => Some("xhigh"),
            v if v < (4, 7) && anthropic_effort_caps(model).supports_reasoning_effort() => {
                Some("high")
            }
            _ => None,
        },
        Some("fable") if matches!(version, (5, 0) | (5, 1)) => Some("high"),
        _ => None,
    }
}

/// Whether a Claude model binds signed thinking to its conversation prefix.
///
/// Each known entry cites its evidence. INT-01 Gate 0 (2026-09-29, Claude
/// OAuth, `T3b`/`T3c`/`T7`) and its WP-01 reproduction measured the check on
/// Opus 5.5 and Sonnet 5.5 and its absence on Opus 5 and Sonnet 5. Anthropic's
/// documentation (preserved thinking, re-verified 2026-09-29) introduces the
/// check with Fable 5.1 (not Fable 5) and exempts Mythos 5.1. Earlier
/// generations predate it. An unknown future generation defaults to
/// `PrefixBound`: over-suppression loses some reasoning but never produces
/// rejections.
pub fn anthropic_reasoning_binding(model: &str) -> ReasoningBinding {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return ReasoningBinding::Unbound;
    }
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return ReasoningBinding::Unbound;
    };
    if version < (5, 0) {
        return ReasoningBinding::Unbound;
    }
    let known = |bound: &[(u32, u32)], unbound: &[(u32, u32)]| {
        if bound.contains(&version) {
            ReasoningBinding::PrefixBound
        } else if unbound.contains(&version) {
            ReasoningBinding::Unbound
        } else {
            ReasoningBinding::PrefixBound
        }
    };
    match family {
        // Measured: Opus 5.5 bound, Opus 5 not.
        Some("opus") => known(&[(5, 5)], &[(5, 0)]),
        // Measured: Sonnet 5.5 bound, Sonnet 5 not.
        Some("sonnet") => known(&[(5, 5)], &[(5, 0)]),
        // Documented: introduced with Fable 5.1.
        Some("fable") => known(&[(5, 1)], &[(5, 0)]),
        // Documented: Mythos 5.1 does not run the check; Mythos 5 predates it.
        Some("mythos") => known(&[], &[(5, 0), (5, 1)]),
        _ => ReasoningBinding::PrefixBound,
    }
}

/// Mid-conversation features a Claude model accepts (INT-01/WP-06, D15 and
/// D17). Policy as data: each entry cites its evidence, and an unknown model
/// gets neither feature until a probe shows it (`jcode provider-doctor claude
/// --contract claude-oauth-boundaries`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnthropicConversationCaps {
    /// A `role: "system"` message inside `messages`: jcode's operator channel.
    pub system_messages: bool,
    /// `tool_addition` (by value, beta `inline-tools-2026-09-15`) and
    /// `tool_removal` blocks in such a message.
    pub inline_tool_changes: bool,
}

/// Mid-conversation capabilities for `model`.
///
/// Measured on Claude OAuth on 2026-10-01 (probes G6.1 and G6.2): Opus 5.5,
/// Sonnet 5.5 and Opus 5 accept both. Sonnet 5 accepted `role: "system"` but
/// rejected tool changes ("tool_addition/tool_removal is not supported on
/// this model"); Anthropic's documentation lists Sonnet 5 as unsupported for
/// system messages, so jcode follows the documentation there and keeps the
/// user form. Documented, not measured: Opus 4.8, Fable 5 and 5.1, Mythos 5
/// and 5.1 support both. Every other model, including future generations,
/// supports neither until measured.
pub fn anthropic_conversation_caps(model: &str) -> AnthropicConversationCaps {
    const BOTH: AnthropicConversationCaps = AnthropicConversationCaps {
        system_messages: true,
        inline_tool_changes: true,
    };
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return AnthropicConversationCaps::default();
    }
    let (family, version) = parse_claude_family_version(&base);
    match (family, version) {
        (Some("opus"), Some((5, 5) | (5, 0) | (4, 8)))
        | (Some("sonnet"), Some((5, 5)))
        | (Some("fable" | "mythos"), Some((5, 0) | (5, 1))) => BOTH,
        _ => AnthropicConversationCaps::default(),
    }
}

/// Effort and thinking-shape capabilities, without the binding policy.
fn anthropic_effort_caps(model: &str) -> AnthropicReasoningCaps {
    let base = normalized_claude_caps_key(model);
    if !base.starts_with("claude") {
        return AnthropicReasoningCaps::NONE;
    }
    if base.contains("mythos") {
        return AnthropicReasoningCaps::EFFORT_NO_XHIGH;
    }
    let (family, version) = parse_claude_family_version(&base);
    let Some(version) = version else {
        return AnthropicReasoningCaps::NONE;
    };
    match family {
        Some("opus") => {
            if version >= (4, 7) {
                AnthropicReasoningCaps::FULL
            } else if version == (4, 6) {
                AnthropicReasoningCaps::EFFORT_NO_XHIGH
            } else if version == (4, 5) {
                AnthropicReasoningCaps::MANUAL_WITH_EFFORT
            } else {
                AnthropicReasoningCaps::NONE
            }
        }
        Some("sonnet") => {
            if version >= (5, 0) {
                AnthropicReasoningCaps::FULL
            } else if version == (4, 6) {
                AnthropicReasoningCaps::EFFORT_NO_XHIGH
            } else if version == (3, 7) {
                AnthropicReasoningCaps::MANUAL_ONLY
            } else {
                AnthropicReasoningCaps::NONE
            }
        }
        // Optimistic default for new generations (Fable 5, Haiku 5, future
        // families): assume the modern full ladder from version 5 on.
        _ => {
            if version >= (5, 0) {
                AnthropicReasoningCaps::FULL
            } else {
                AnthropicReasoningCaps::NONE
            }
        }
    }
}

/// Whether `name` satisfies the Messages API tool-name rule
/// `^[a-zA-Z0-9_-]{1,128}$`.
pub fn anthropic_tool_name_is_valid(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn anthropic_stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

pub fn anthropic_stainless_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "MacOS",
        "windows" => "Windows",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn conversation_caps_follow_the_measured_and_documented_models() {
        use super::anthropic_conversation_caps as caps;
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-mythos-5-1",
            "claude-opus-5-5[1m]",
        ] {
            assert!(
                caps(model).system_messages && caps(model).inline_tool_changes,
                "{model}"
            );
        }
        for model in [
            "claude-sonnet-5",
            "claude-opus-4-7",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-opus-6",
            "claude-sonnet-6-0",
            "gpt-5.6-sol",
        ] {
            assert_eq!(
                caps(model),
                super::AnthropicConversationCaps::default(),
                "{model}"
            );
        }
    }

    use super::*;
    use crate::ALL_CLAUDE_MODELS;

    #[test]
    fn model_suffix_helpers_require_explicit_1m_suffix() {
        assert!(!anthropic_effectively_1m("claude-opus-4-6"));
        assert!(anthropic_effectively_1m("claude-opus-4-6[1m]"));
        assert_eq!(
            anthropic_strip_1m_suffix("claude-opus-4-6[1m]"),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn oauth_beta_headers_follow_1m_suffix() {
        assert_eq!(
            anthropic_oauth_beta_headers("claude-opus-4-6"),
            ANTHROPIC_OAUTH_BETA_HEADERS
        );
        assert_eq!(
            anthropic_oauth_beta_headers("claude-opus-4-6[1m]"),
            ANTHROPIC_OAUTH_BETA_HEADERS_1M
        );
    }

    #[test]
    fn tool_name_rule_matches_the_messages_api_pattern() {
        assert!(anthropic_tool_name_is_valid("bash"));
        assert!(anthropic_tool_name_is_valid("mcp__server__tool-name_2"));
        assert!(anthropic_tool_name_is_valid(&"a".repeat(128)));
        assert!(!anthropic_tool_name_is_valid(""));
        assert!(!anthropic_tool_name_is_valid(&"a".repeat(129)));
        assert!(!anthropic_tool_name_is_valid("functions.bash"));
        assert!(!anthropic_tool_name_is_valid("has space"));
    }

    #[test]
    fn stainless_labels_are_non_empty() {
        assert!(!anthropic_stainless_arch().is_empty());
        assert!(!anthropic_stainless_os().is_empty());
    }

    #[test]
    fn current_claude_models_are_listed_and_classified() {
        // INT-01 E3: the static list, context classification and output budget
        // know the current generations.
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"] {
            assert!(ALL_CLAUDE_MODELS.contains(&model), "{model} listed");
            assert_eq!(
                anthropic_context_mode(model),
                AnthropicContextMode::Native1M,
                "{model}"
            );
            assert_eq!(anthropic_max_output_tokens(model), 128_000, "{model}");
            assert!(anthropic_reasoning_caps(model).supports_reasoning_effort());
            assert!(
                crate::pricing::anthropic_api_pricing(model).is_some(),
                "{model} is priced"
            );
        }
        assert_eq!(ALL_CLAUDE_MODELS[0], crate::DEFAULT_CLAUDE_MODEL);
    }

    #[test]
    fn default_effort_is_an_explicit_per_model_table() {
        // INT-01 DESIGN §7 (E1): Opus 5.5 is no longer caught by an Opus 5
        // prefix match.
        for (model, expected) in [
            ("claude-opus-5-5", Some("medium")),
            ("claude-opus-5-5[1m]", Some("medium")),
            ("claude-opus-5", Some("low")),
            ("claude-opus-4-8", Some("xhigh")),
            ("claude-opus-4-7", Some("xhigh")),
            ("claude-opus-4-6", Some("high")),
            ("claude-opus-4-5", Some("high")),
            ("claude-fable-5-1", Some("high")),
            ("claude-fable-5", Some("high")),
            ("claude-sonnet-5-5", None),
            ("claude-sonnet-5", None),
            ("claude-sonnet-4-6", None),
            ("claude-haiku-4-5", None),
            ("claude-mythos-5-1", None),
            ("claude-opus-4-1", None),
            ("claude-opus-6", None),
            ("claude-fable-5-2", None),
            ("gpt-5.6-sol", None),
        ] {
            assert_eq!(
                anthropic_default_reasoning_effort(model),
                expected,
                "{model}"
            );
        }
    }

    #[test]
    fn thinking_off_follows_each_generation() {
        use ThinkingOff::*;
        for (model, expected) in [
            ("claude-opus-5-5", AlwaysOn),
            ("claude-sonnet-5-5", AlwaysOn),
            ("claude-fable-5-1", AlwaysOn),
            ("claude-fable-5", AlwaysOn),
            ("claude-mythos-5-1", AlwaysOn),
            ("claude-opus-5", Disabled),
            ("claude-sonnet-5", Disabled),
            ("claude-sonnet-5-20260701", Disabled),
            ("claude-opus-4-8", OmitThinking),
            ("claude-opus-4-7", OmitThinking),
            ("claude-sonnet-4-6", OmitThinking),
            ("claude-haiku-4-5", OmitThinking),
            ("claude-opus-6", AlwaysOn),
        ] {
            assert_eq!(anthropic_thinking_off(model), expected, "{model}");
            assert_eq!(
                anthropic_reasoning_caps(model).thinking_off,
                expected,
                "{model}"
            );
        }
    }

    #[test]
    fn sampling_parameters_are_sent_only_where_documented() {
        for model in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-mythos-5-1",
            "claude-opus-6",
        ] {
            assert!(!anthropic_accepts_sampling_parameters(model), "{model}");
            assert!(
                !anthropic_reasoning_caps(model).sampling_parameters,
                "{model}"
            );
        }
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-3-7-sonnet",
        ] {
            assert!(anthropic_accepts_sampling_parameters(model), "{model}");
        }
    }

    #[test]
    fn reasoning_caps_match_live_verified_generations() {
        // Full ladder: Fable 5 (live 2026-07-01), Sonnet 5 (live 2026-07-07),
        // Opus 5 (live 2026-07-24), Opus 4.7/4.8.
        for model in [
            "claude-fable-5",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
        ] {
            let caps = anthropic_reasoning_caps(model);
            assert!(caps.output_effort, "{model} should support output effort");
            assert!(caps.adaptive_thinking, "{model} should be adaptive");
            assert!(caps.xhigh_effort, "{model} should support xhigh");
            assert!(caps.max_effort, "{model} should support max");
            assert!(!caps.manual_thinking);
        }

        // Effort without xhigh: Opus/Sonnet 4.6, Mythos.
        for model in ["claude-opus-4-6", "claude-sonnet-4-6", "claude-mythos"] {
            let caps = anthropic_reasoning_caps(model);
            assert!(caps.output_effort, "{model} should support output effort");
            assert!(caps.adaptive_thinking);
            assert!(!caps.xhigh_effort, "{model} has no xhigh");
            assert!(caps.max_effort, "{model} still supports max");
        }

        // Manual thinking generations.
        let opus_4_5 = anthropic_reasoning_caps("claude-opus-4-5");
        assert!(opus_4_5.output_effort && opus_4_5.manual_thinking);
        assert!(!opus_4_5.adaptive_thinking && !opus_4_5.xhigh_effort && !opus_4_5.max_effort);
        let sonnet_3_7 = anthropic_reasoning_caps("claude-3-7-sonnet");
        assert!(sonnet_3_7.manual_thinking && !sonnet_3_7.output_effort);
        assert_eq!(
            anthropic_reasoning_caps("claude-sonnet-3-7"),
            sonnet_3_7,
            "version-first and version-last forms must match"
        );

        // No reasoning-effort support.
        for model in [
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-opus-4-1",
            "claude-3-5-haiku",
            "gpt-5.5",
        ] {
            assert!(
                !anthropic_reasoning_caps(model).supports_reasoning_effort(),
                "{model} should not support effort"
            );
        }
    }

    #[test]
    fn max_output_tokens_match_published_model_limits() {
        // 128K-output generations, including dotted and [1m]/dated aliases.
        for model in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4.8",
            "claude-opus-4-7",
            "claude-opus-4-6[1m]",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "Claude-Opus-5-20260724",
        ] {
            assert_eq!(
                anthropic_max_output_tokens(model),
                128_000,
                "{model} should allow 128K output"
            );
        }

        // Haiku 4.5 is a 64K-output model.
        assert_eq!(anthropic_max_output_tokens("claude-haiku-4-5"), 64_000);
        assert_eq!(
            anthropic_max_output_tokens("claude-haiku-4-5-20251001"),
            64_000
        );

        // Older generations keep the conservative legacy budget.
        for model in [
            "claude-opus-4-5",
            "claude-sonnet-4-5",
            "claude-sonnet-4-20250514",
            "claude-instant",
        ] {
            assert_eq!(
                anthropic_max_output_tokens(model),
                32_768,
                "{model} should keep the conservative default"
            );
        }
    }

    #[test]
    fn max_output_tokens_never_undercut_the_legacy_default() {
        // Regression guard: a per-model budget must never be *smaller* than the
        // flat 32K default jcode shipped before, or turns that used to fit would
        // start truncating.
        for model in ALL_CLAUDE_MODELS {
            assert!(
                anthropic_max_output_tokens(model) >= 32_768,
                "{model} regressed below the legacy 32K output budget"
            );
        }
    }

    #[test]
    fn reasoning_caps_normalize_suffixes_and_dots() {
        let base = anthropic_reasoning_caps("claude-sonnet-5");
        assert_eq!(anthropic_reasoning_caps("claude-sonnet-5[1m]"), base);
        assert_eq!(anthropic_reasoning_caps("claude-sonnet-5-20260701"), base);
        assert_eq!(anthropic_reasoning_caps("Claude-Sonnet-5"), base);
        assert_eq!(
            anthropic_reasoning_caps("claude-opus-4.6"),
            anthropic_reasoning_caps("claude-opus-4-6")
        );
    }

    #[test]
    fn reasoning_caps_are_optimistic_for_future_generations() {
        // New 5.x+ models default to the full ladder (the runtime self-heals
        // on 400 by stripping reasoning fields), instead of silently
        // disabling effort until someone probes them.
        for model in [
            "claude-sonnet-5-1",
            "claude-sonnet-6",
            "claude-opus-5",
            "claude-haiku-5",
            "claude-fable-6",
            "claude-nova-5",
        ] {
            // Effort capabilities only: the binding policy is a separate,
            // per-model entry (INT-01 D11).
            assert_eq!(
                anthropic_effort_caps(model),
                anthropic_effort_caps("claude-fable-5"),
                "{model} should default to the full modern ladder"
            );
        }
        // But old/unversioned ids stay conservative.
        assert!(!anthropic_reasoning_caps("claude-haiku-4-5").supports_reasoning_effort());
        assert!(!anthropic_reasoning_caps("claude-instant").supports_reasoning_effort());
    }

    #[test]
    fn reasoning_binding_entries_follow_their_evidence() {
        use ReasoningBinding::{PrefixBound, Unbound};
        for (model, expected) in [
            // Measured, INT-01 Gate 0 and WP-01 (Claude OAuth, 2026-09-29).
            ("claude-opus-5-5", PrefixBound),
            ("claude-opus-5-5[1m]", PrefixBound),
            ("claude-sonnet-5-5", PrefixBound),
            ("claude-opus-5", Unbound),
            ("claude-sonnet-5", Unbound),
            // Documented.
            ("claude-fable-5-1", PrefixBound),
            ("claude-fable-5", Unbound),
            ("claude-mythos-5-1", Unbound),
            ("claude-mythos-5", Unbound),
            // Predate preserved thinking.
            ("claude-opus-4-8", Unbound),
            ("claude-sonnet-4-6", Unbound),
            ("claude-haiku-4-5", Unbound),
            ("claude-3-7-sonnet", Unbound),
            // Unknown future generations default to PrefixBound.
            ("claude-opus-6", PrefixBound),
            ("claude-sonnet-5-7", PrefixBound),
            ("claude-fable-5-2", PrefixBound),
            ("claude-mythos-5-2", PrefixBound),
            ("claude-haiku-5", PrefixBound),
            // Not a Claude model.
            ("gpt-5.6-sol", Unbound),
        ] {
            assert_eq!(anthropic_reasoning_binding(model), expected, "{model}");
            assert_eq!(
                anthropic_reasoning_caps(model).reasoning_binding,
                expected,
                "{model}: the capability entry and the table agree"
            );
        }
    }

    #[test]
    fn the_binding_policy_does_not_change_effort_capabilities() {
        for model in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-mythos-5-1",
            "claude-3-7-sonnet",
        ] {
            // The per-model policy entries (binding, thinking off, sampling)
            // leave the effort ladder as the effort table defines it.
            let policy_free = |caps: AnthropicReasoningCaps| AnthropicReasoningCaps {
                reasoning_binding: ReasoningBinding::Unbound,
                thinking_off: ThinkingOff::OmitThinking,
                sampling_parameters: true,
                ..caps
            };
            assert_eq!(
                policy_free(anthropic_reasoning_caps(model)),
                policy_free(anthropic_effort_caps(model)),
                "{model}"
            );
        }
    }
}
