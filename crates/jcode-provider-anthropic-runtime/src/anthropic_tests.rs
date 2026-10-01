use super::*;

/// The binding the runtime would compute for a request whose last replayed
/// thinking block has fingerprint `last_thinking`.
fn test_request_binding(last_thinking: Option<&str>) -> RequestBinding {
    RequestBinding {
        prefix_digest: "anthropic-prefix-v1:request".to_string(),
        last_thinking: last_thinking.map(str::to_string),
    }
}

fn test_sse_state() -> SseStreamState {
    SseStreamState::new("claude-opus-5-5", test_request_binding(None))
}

fn sse(event_type: &str, data: serde_json::Value) -> SseEvent {
    SseEvent {
        event_type: event_type.to_string(),
        data: data.to_string(),
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::set_var(key, value);
        Self { key, previous }
    }

    fn set_if_missing(key: &'static str, value: &str) -> Option<Self> {
        if std::env::var_os(key).is_some() {
            return None;
        }
        Some(Self::set(key, value))
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            jcode_base::env::set_var(self.key, previous);
        } else {
            jcode_base::env::remove_var(self.key);
        }
    }
}

async fn collect_live_smoke_stream(
    mut stream: EventStream,
    timeout: std::time::Duration,
) -> Result<(usize, usize, bool)> {
    tokio::time::timeout(timeout, async move {
        let mut text_bytes = 0usize;
        let mut thinking_bytes = 0usize;
        let mut saw_message_end = false;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(text) => {
                    text_bytes += text.len();
                }
                StreamEvent::ThinkingDelta(text) => {
                    thinking_bytes += text.len();
                }
                StreamEvent::MessageEnd { .. } => {
                    saw_message_end = true;
                    break;
                }
                StreamEvent::Error { message, .. } => anyhow::bail!(message),
                _ => {}
            }
        }
        Ok((text_bytes, thinking_bytes, saw_message_end))
    })
    .await
    .context("live provider smoke timed out")?
}

#[test]
fn test_parse_sse_event() {
    let mut buffer = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string();
    let event = parse_sse_event(&mut buffer).unwrap();
    assert_eq!(event.event_type, "message_start");
    assert!(buffer.is_empty());
}

#[tokio::test]
async fn test_available_models() {
    let provider = AnthropicProvider::new();
    let models = provider.available_models();
    assert!(models.contains(&"claude-opus-4-8"));
    // Opus 4.8 is native-1M, so there is no redundant `[1m]` alias.
    assert!(!models.contains(&"claude-opus-4-8[1m]"));
    assert!(models.contains(&"claude-opus-4-6"));
    assert!(models.contains(&"claude-opus-4-6[1m]"));
    assert!(models.contains(&"claude-sonnet-4-6"));
    assert!(models.contains(&"claude-sonnet-4-6[1m]"));
    assert!(models.contains(&"claude-haiku-4-5"));
}

#[test]
fn test_effectively_1m_requires_explicit_suffix() {
    assert!(!effectively_1m("claude-opus-4-6"));
    assert!(!effectively_1m("claude-sonnet-4-6"));
    assert!(effectively_1m("claude-opus-4-6[1m]"));
    assert!(effectively_1m("claude-sonnet-4-6[1m]"));
}

#[test]
fn test_oauth_beta_headers_require_explicit_1m_suffix() {
    assert_eq!(oauth_beta_headers("claude-opus-4-6"), OAUTH_BETA_HEADERS);
    assert_eq!(
        oauth_beta_headers("claude-opus-4-6[1m]"),
        OAUTH_BETA_HEADERS_1M
    );
}

#[test]
fn test_anthropic_reasoning_effort_request_parts() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();
    provider.set_reasoning_effort("none").unwrap();
    assert!(
        provider.set_reasoning_effort("minimal").is_err(),
        "Anthropic must reject rather than silently promote minimal to max"
    );

    assert_eq!(
        provider.available_efforts(),
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
    assert_eq!(provider.reasoning_effort().as_deref(), Some("none"));

    // Sonnet 4.6 supports the real `max` API level (but not `xhigh`).
    provider.set_reasoning_effort("max").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("max"));

    // `xhigh` is rejected on models that do not support it.
    assert!(provider.set_reasoning_effort("xhigh").is_err());

    provider.set_reasoning_effort("medium").unwrap();
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-sonnet-4-6", true);

    match thinking.expect("adaptive thinking should be enabled") {
        ApiThinking::Adaptive { display, .. } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Claude 4.6 should use adaptive thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }
    assert_eq!(
        output_config.expect("output_config should be set").effort,
        "medium"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_preserves_swarm_sentinels_for_cycling() {
    // Regression: storing a swarm effort must preserve which swarm mode was
    // chosen. Previously both `swarm` and `swarm-deep` collapsed to `swarm`,
    // which capped Alt+Right effort cycling at swarm-light (it could never
    // reach swarm-deep because the readback always reported `swarm`).
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();

    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));

    provider.set_reasoning_effort("swarm-deep").unwrap();
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("swarm-deep"),
        "swarm-deep must survive the round-trip so cycling can reach it"
    );

    // And cycling back down to swarm-light still works.
    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));
}

#[test]
fn test_anthropic_show_thinking_enables_adaptive_thinking_without_effort() {
    // With no explicit reasoning effort, an adaptive-thinking model should still
    // request summarized thinking when the user has opted into the display.
    // Crucially, `output_config` must stay None so we do not force a stronger
    // (more expensive) reasoning level than the model's default.
    //
    // We use a non-Opus model here because Opus now carries an implicit `xhigh`
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`); Sonnet keeps
    // the model's own default so this invariant stays meaningful.
    //
    // `build_reasoning_request_parts_inner` takes the model directly, so we do
    // not depend on `set_model` accepting a particular catalog entry. With no
    // effort configured, `self.reasoning_effort()` resolves to None regardless
    // of the default model.
    let provider = AnthropicProvider::new();
    // Make the test independent of the ambient config's anthropic_reasoning_effort
    // by clearing the field directly; we only exercise the show_thinking path.
    *provider.reasoning_effort.write().unwrap() = None;

    // show_thinking = false: nothing requested.
    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-sonnet-4-6",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert!(
        thinking.is_none(),
        "no thinking should be requested when both effort and show_thinking are off"
    );
    assert!(output_config.is_none());

    // show_thinking = true: adaptive thinking requested, no output_config.
    let (thinking, output_config, temperature) = provider.build_reasoning_request_parts_inner(
        "claude-sonnet-4-6",
        true,
        true,
        PrefixMismatchBehavior::Error,
    );
    match thinking.expect("show_thinking should enable adaptive thinking") {
        ApiThinking::Adaptive { display, .. } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Sonnet 4.6 should use adaptive thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }
    assert!(
        output_config.is_none(),
        "show_thinking alone must not force an output reasoning effort"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_explicit_none_effort_disables_thinking_even_with_show_thinking() {
    // Regression: with `display.show_thinking = true` (the default), setting
    // effort to `none` still requested adaptive thinking. An explicit `none`
    // must turn thinking off wherever the model allows it, on adaptive and
    // manual thinking models alike (INT-01 DESIGN §7).
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("none".to_string());

    // Adaptive-thinking model that runs without thinking when it is omitted.
    let (thinking, output_config, temperature) = provider.build_reasoning_request_parts_inner(
        "claude-sonnet-4-6",
        true,
        true,
        PrefixMismatchBehavior::Error,
    );
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress thinking even when show_thinking is on"
    );
    assert!(output_config.is_none());
    assert_eq!(
        temperature,
        Some(1.0),
        "no thinking means the OAuth path restores temperature where accepted"
    );

    // Manual-thinking model.
    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-3-7-sonnet",
        false,
        true,
        PrefixMismatchBehavior::Error,
    );
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress manual thinking budgets too"
    );
    assert!(output_config.is_none());
}

#[test]
fn effort_none_follows_how_each_generation_turns_thinking_off() {
    // INT-01 DESIGN §7, R13.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("none".to_string());
    let parts = |model: &str| {
        provider.build_reasoning_request_parts_inner(
            model,
            true,
            true,
            PrefixMismatchBehavior::DropBlock,
        )
    };

    // Thinking on unless disabled: `{type: "disabled"}`, no effort, and no
    // sampling parameter.
    for model in ["claude-opus-5", "claude-sonnet-5"] {
        let (thinking, output_config, temperature) = parts(model);
        assert_eq!(
            serde_json::to_value(thinking.as_ref().expect("disabled")).unwrap(),
            json!({"type": "disabled"}),
            "{model}"
        );
        assert!(output_config.is_none(), "{model}");
        assert_eq!(temperature, None, "{model} rejects sampling parameters");
    }

    // Thinking cannot be turned off: `none` is `low`.
    for model in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable-5-1",
        "claude-fable-5",
    ] {
        let (thinking, output_config, temperature) = parts(model);
        assert!(
            matches!(thinking, Some(ApiThinking::Adaptive { .. })),
            "{model} keeps adaptive thinking"
        );
        assert_eq!(output_config.expect("effort").effort, "low", "{model}");
        assert_eq!(temperature, None, "{model}");
    }

    // Omitting thinking turns it off; Opus 4.8 takes no sampling parameter.
    let (thinking, output_config, temperature) = parts("claude-opus-4-8");
    assert!(thinking.is_none() && output_config.is_none());
    assert_eq!(temperature, None);
}

#[test]
fn effort_none_is_not_offered_where_thinking_cannot_be_turned_off() {
    let provider = AnthropicProvider::new();
    use_model(&provider, "claude-opus-5-5");
    assert!(!provider.available_efforts().contains(&"none"));
    provider.set_reasoning_effort("none").unwrap();
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("low"),
        "the stored and surfaced effort is the one sent"
    );
    use_model(&provider, "claude-opus-5");
    assert!(provider.available_efforts().contains(&"none"));
}

#[test]
fn unconfigured_efforts_follow_the_default_table() {
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;
    let parts = |model: &str| {
        provider.build_reasoning_request_parts_inner(
            model,
            true,
            false,
            PrefixMismatchBehavior::DropBlock,
        )
    };

    let (thinking, output_config, temperature) = parts("claude-opus-5-5");
    assert_eq!(output_config.expect("effort").effort, "medium");
    assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
    assert_eq!(temperature, None);

    // Sonnet 5.5 keeps its model default: no effort, and thinking carries only
    // the binding control (the model always thinks).
    let (thinking, output_config, temperature) = parts("claude-sonnet-5-5");
    assert!(output_config.is_none());
    assert_eq!(
        serde_json::to_value(thinking.expect("binding control")).unwrap(),
        json!({"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "drop_block"}})
    );
    assert_eq!(temperature, None);

    // Sonnet 5 keeps its model default and receives no sampling parameter.
    let (thinking, output_config, temperature) = parts("claude-sonnet-5");
    assert!(thinking.is_none() && output_config.is_none());
    assert_eq!(temperature, None);
}

#[test]
fn test_anthropic_fable_defaults_to_high_effort() {
    // Fable 5 defaults to `high` reasoning when no explicit user effort is
    // configured. An explicit override still wins.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-fable-5").as_deref(),
        Some("high"),
    );

    // The default drives the request: output_config high + adaptive thinking.
    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-fable-5",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert_eq!(
        output_config
            .expect("Fable should default to a forced output effort")
            .effort,
        "high",
    );
    match thinking.expect("Fable default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display, .. } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Fable 5 should use adaptive thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-fable-5".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));

    // An explicit user override still wins over the Fable default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    // Fable always thinks, so `none` means its lowest setting and beats the
    // high default (INT-01 DESIGN §7).
    provider.set_reasoning_effort("none").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-fable-5",
        true,
        true,
        PrefixMismatchBehavior::Error,
    );
    assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
    assert_eq!(output_config.expect("effort").effort, "low");
}

#[test]
fn test_anthropic_sonnet_5_supports_full_effort_ladder() {
    // `claude-sonnet-5` accepts `output_config` effort low..xhigh/max and
    // adaptive thinking (verified live 2026-07-07).
    assert!(jcode_provider_core::anthropic_reasoning_caps("claude-sonnet-5").output_effort);
    assert!(jcode_provider_core::anthropic_reasoning_caps("claude-sonnet-5").adaptive_thinking);
    assert!(AnthropicProvider::model_supports_xhigh_effort(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_max_effort(
        "claude-sonnet-5"
    ));

    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-sonnet-5".to_string();

    // No forced default: Sonnet keeps the model's own reasoning behavior.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-5"),
        None,
    );

    // Explicit efforts are accepted and drive the request.
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        provider.set_reasoning_effort(effort).unwrap();
        assert_eq!(provider.reasoning_effort().as_deref(), Some(effort));
        let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
            "claude-sonnet-5",
            true,
            false,
            PrefixMismatchBehavior::Error,
        );
        assert_eq!(
            output_config
                .expect("explicit effort should set output_config")
                .effort,
            effort,
        );
        assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
    }

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "swarm",
            "swarm-deep"
        ],
    );
}

#[test]
fn test_anthropic_opus_defaults_to_xhigh_effort() {
    // Opus is a reasoning-heavy flagship, so when the user has *not* configured
    // an explicit effort it should default to its strongest supported level
    // (`xhigh` on Opus 4.7/4.8). This drives both the request `output_config`
    // and the surfaced `reasoning_effort()` status.
    let provider = AnthropicProvider::new();
    // Clear any ambient config-provided effort so we exercise the model default.
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-8").as_deref(),
        Some("xhigh"),
    );
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-7").as_deref(),
        Some("xhigh"),
    );
    // Older Opus does not support xhigh, so it clamps to high.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-5").as_deref(),
        Some("high"),
    );
    // Non-Opus models keep the model's own default (no forced effort).
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-4-6"),
        None,
    );

    // Even without show_thinking, Opus forces its strongest output effort.
    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-opus-4-8",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert_eq!(
        output_config
            .expect("Opus should default to a forced output effort")
            .effort,
        "xhigh",
    );
    match thinking.expect("Opus default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display, .. } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Opus 4.8 should use adaptive thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-8".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));

    // An explicit user override still wins over the Opus default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
}

#[test]
fn test_anthropic_show_thinking_enables_manual_thinking_without_effort() {
    // Manual-thinking models (e.g. Claude 3.7 Sonnet) need a concrete budget;
    // with only the display toggle on we fall back to the minimal budget. We use
    // a non-Opus model here because Opus now carries an implicit strongest-effort
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`). The model is
    // passed directly so this does not depend on `set_model` validation.
    let provider = AnthropicProvider::new();
    // Independent of ambient config: clear any configured effort.
    *provider.reasoning_effort.write().unwrap() = None;

    let (thinking, _output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-3-7-sonnet",
        false,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert!(thinking.is_none());

    let (thinking, _output_config, _temperature) = provider.build_reasoning_request_parts_inner(
        "claude-3-7-sonnet",
        false,
        true,
        PrefixMismatchBehavior::Error,
    );
    match thinking.expect("show_thinking should enable manual thinking") {
        ApiThinking::Enabled { budget_tokens, .. } => assert_eq!(budget_tokens, 1_024),
        ApiThinking::Adaptive { .. } => panic!("Claude 3.7 Sonnet should use manual thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }
}

#[test]
fn test_anthropic_max_alias_uses_strongest_real_effort() {
    // `max` is a real API level on output_config effort models.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-7", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "max"),
        "max"
    );
    // Manual-thinking models (no output_config) clamp max to high.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "max"),
        "high"
    );
    // xhigh still clamps to high where unsupported.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "xhigh"),
        "high"
    );
    // Swarm rungs pin to the strongest supported level.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "swarm"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "swarm-deep"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "swarm"),
        "high"
    );
}

#[test]
fn test_anthropic_opus_48_fast_mode_service_tier_serializes_priority() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-8").unwrap();

    assert_eq!(provider.available_service_tiers(), vec!["off", "priority"]);
    assert_eq!(provider.service_tier(), None);

    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    let request = ApiRequest {
        model: strip_1m_suffix(&provider.model()).to_string(),
        max_tokens: 1024,
        system: None,
        messages: vec![],
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: provider.current_service_tier_for_model(&provider.model()),
        stream: true,
    };
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["model"], "claude-opus-4-8");
    assert_eq!(value["service_tier"], "auto");
}

#[test]
fn test_anthropic_fast_mode_is_limited_to_opus_48() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-6").unwrap();

    assert!(provider.available_service_tiers().is_empty());
    assert!(provider.set_service_tier("priority").is_err());
    assert_eq!(provider.service_tier(), None);

    // A stale `[1m]` alias for a native-1M model is migrated to canonical form.
    provider.set_model("claude-opus-4-8[1m]").unwrap();
    assert_eq!(provider.model(), "claude-opus-4-8");
    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    provider.set_service_tier("off").unwrap();
    assert_eq!(provider.service_tier(), None);
}

#[test]
fn test_anthropic_manual_thinking_budget_for_opus_45() {
    let provider = AnthropicProvider::new();
    // Keep this request-builder test independent of the live/persisted Anthropic
    // model catalog, which may legitimately omit older Opus 4.5 models.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-5".to_string();
    provider.set_reasoning_effort("high").unwrap();

    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-opus-4-5", false);

    match thinking.expect("manual thinking should be enabled") {
        ApiThinking::Enabled { budget_tokens, .. } => assert_eq!(budget_tokens, 8_192),
        ApiThinking::Adaptive { .. } => panic!("Claude Opus 4.5 should use manual thinking"),
        ApiThinking::Disabled => panic!("thinking must not be disabled here"),
    }
    assert_eq!(output_config.unwrap().effort, "high");
    assert_eq!(temperature, None);
}

#[test]
fn message_start_warns_when_server_substitutes_a_different_model() {
    // Anthropic can silently alias an unavailable model id to a different model
    // (observed: claude-fable-5 -> claude-haiku-4-5). When the served model
    // differs from the requested base id, we must surface a StatusDetail warning
    // so the user is not misled about which model answered.
    let mut state = SseStreamState::new("claude-fable-5", test_request_binding(None));
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-haiku-4-5-20251001", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state);
    let warned = events.iter().any(|e| {
        matches!(e, StreamEvent::StatusDetail { detail }
            if detail.contains("claude-haiku-4-5") && detail.contains("claude-fable-5"))
    });
    assert!(
        warned,
        "expected a substitution StatusDetail, got {events:?}"
    );
    assert!(state.warned_model_substitution);

    // A matching served model must NOT warn.
    let mut state = SseStreamState::new("claude-opus-4-8", test_request_binding(None));
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-opus-4-8", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::StatusDetail { .. })),
        "served model matched request; must not warn"
    );
    assert!(!state.warned_model_substitution);
}

#[test]
fn test_anthropic_thinking_sse_events() {
    let mut state = SseStreamState::new(
        "claude-opus-5-5",
        test_request_binding(Some("request-last")),
    );
    let events = process_sse_event(
        &sse(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "thinking", "thinking": "", "signature": ""}
            }),
        ),
        &mut state,
    );
    assert!(matches!(events.as_slice(), [StreamEvent::ThinkingStart]));
    assert!(state.current_thinking.is_some());

    for text in ["reasoning ", "text"] {
        let events = process_sse_event(
            &sse(
                "content_block_delta",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "thinking_delta", "thinking": text}
                }),
            ),
            &mut state,
        );
        assert!(matches!(events.as_slice(), [StreamEvent::ThinkingDelta(t)] if t == text));
    }

    let events = process_sse_event(
        &sse(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "signature_delta", "signature": "signed"}
            }),
        ),
        &mut state,
    );
    assert!(
        events.is_empty(),
        "the signature is carried by the finished block"
    );

    let events = process_sse_event(
        &sse(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ),
        &mut state,
    );
    match events.as_slice() {
        [
            StreamEvent::ReplayableReasoning(ReplayableReasoningBlock::AnthropicThinking {
                thinking,
                signature,
                binding,
            }),
            StreamEvent::ThinkingEnd,
        ] => {
            assert_eq!(thinking, "reasoning text");
            assert_eq!(signature, "signed");
            assert_eq!(binding.model, "claude-opus-5-5");
            assert_eq!(binding.prefix_digest, "anthropic-prefix-v1:request");
            assert_eq!(binding.predecessor.as_deref(), Some("request-last"));
        }
        other => panic!("expected a finished signed block, got {other:?}"),
    }
    assert!(state.current_thinking.is_none());
}

#[test]
fn every_thinking_block_is_captured_separately_and_chained_in_stream_order() {
    let mut state = test_sse_state();
    let mut blocks = Vec::new();
    let mut run = |event_type: &str, data: serde_json::Value, state: &mut SseStreamState| {
        for event in process_sse_event(&sse(event_type, data), state) {
            if let StreamEvent::ReplayableReasoning(block) = event {
                blocks.push(block);
            }
        }
    };
    run(
        "message_start",
        serde_json::json!({"type": "message_start", "message": {"model": "claude-opus-5-5-20260901"}}),
        &mut state,
    );
    // Signed block with text, then a redacted block, then an empty-text
    // signed block (a progress update under `display: "omitted"`).
    run(
        "content_block_start",
        serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "a"}}),
        &mut state,
    );
    run(
        "content_block_delta",
        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-a"}}),
        &mut state,
    );
    run(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        &mut state,
    );
    run(
        "content_block_start",
        serde_json::json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque"}}),
        &mut state,
    );
    run(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": 1}),
        &mut state,
    );
    run(
        "content_block_start",
        serde_json::json!({"type": "content_block_start", "index": 2, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        &mut state,
    );
    run(
        "content_block_delta",
        serde_json::json!({"type": "content_block_delta", "index": 2, "delta": {"type": "signature_delta", "signature": "sig-c"}}),
        &mut state,
    );
    run(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": 2}),
        &mut state,
    );

    assert_eq!(blocks.len(), 3, "{blocks:?}");
    let fingerprint_a = thinking_fingerprint(ThinkingPayload::Signature, "sig-a");
    let fingerprint_b = thinking_fingerprint(ThinkingPayload::RedactedData, "opaque");
    match (&blocks[0], &blocks[1], &blocks[2]) {
        (
            ReplayableReasoningBlock::AnthropicThinking {
                thinking: a,
                signature: sig_a,
                binding: first,
            },
            ReplayableReasoningBlock::AnthropicRedactedThinking {
                data,
                binding: second,
            },
            ReplayableReasoningBlock::AnthropicThinking {
                thinking: c,
                signature: sig_c,
                binding: third,
            },
        ) => {
            assert_eq!((a.as_str(), sig_a.as_str()), ("a", "sig-a"));
            assert_eq!(data, "opaque");
            assert_eq!((c.as_str(), sig_c.as_str()), ("", "sig-c"));
            assert_eq!(first.predecessor, None);
            assert_eq!(second.predecessor.as_ref(), Some(&fingerprint_a));
            assert_eq!(third.predecessor.as_ref(), Some(&fingerprint_b));
            assert_eq!(
                first.model, "claude-opus-5-5-20260901",
                "the served model is recorded"
            );
        }
        other => panic!("unexpected blocks {other:?}"),
    }
}

#[test]
fn unsigned_thinking_is_display_only() {
    let mut state = test_sse_state();
    process_sse_event(
        &sse(
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "x"}}),
        ),
        &mut state,
    );
    let events = process_sse_event(
        &sse(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ),
        &mut state,
    );
    assert!(
        matches!(events.as_slice(), [StreamEvent::ThinkingEnd]),
        "{events:?}"
    );
}

#[test]
fn input_transformations_are_recorded_and_prefix_mismatches_are_surfaced() {
    let mut state = test_sse_state();
    let events = process_sse_event(
        &sse(
            "message_start",
            serde_json::json!({"type": "message_start", "message": {
                "model": "claude-opus-5-5",
                "input_transformations": [
                    {"type": "thinking_dropped", "path": "messages.1.content.0", "reason": "prefix_binding_mismatch"},
                    {"type": "thinking_dropped", "path": "messages.3.content.0", "reason": "model_binding_mismatch"},
                    {"type": "some_future_type", "reason": "some_future_reason"}
                ]
            }}),
        ),
        &mut state,
    );
    let notices: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::StatusDetail { detail } => Some(detail),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1, "{events:?}");
    assert!(
        notices[0].contains("1 earlier thinking block"),
        "{}",
        notices[0]
    );
    let counted = jcode_provider_core::anthropic_binding_diagnostics::snapshot();
    for (kind, reason) in [
        ("thinking_dropped", "prefix_binding_mismatch"),
        ("thinking_dropped", "model_binding_mismatch"),
        ("some_future_type", "some_future_reason"),
    ] {
        assert!(
            counted
                .iter()
                .any(|entry| entry.kind == kind && entry.reason == reason && entry.count > 0),
            "{kind}/{reason} not counted: {counted:?}"
        );
    }

    let empty = process_sse_event(
        &sse(
            "message_start",
            serde_json::json!({"type": "message_start", "message": {"input_transformations": []}}),
        ),
        &mut state,
    );
    assert!(
        empty.is_empty(),
        "an empty array is the normal case: {empty:?}"
    );
}

#[test]
fn test_anthropic_signed_thinking_replayed_in_request_blocks() {
    let provider = AnthropicProvider::new();
    let blocks = provider.format_content_blocks(&[ContentBlock::AnthropicThinking {
        thinking: "reasoning text".to_string(),
        signature: "signed".to_string(),
        binding: Some(jcode_message_types::AnthropicThinkingBinding {
            model: "claude-test".to_string(),
            prefix_digest: "anthropic-prefix-v1:test".to_string(),
            predecessor: None,
        }),
    }]);

    let value = serde_json::to_value(&blocks).expect("serialize content blocks");
    assert_eq!(
        value,
        serde_json::json!([
            {
                "type": "thinking",
                "thinking": "reasoning text",
                "signature": "signed"
            }
        ])
    );
}

#[tokio::test]
#[ignore = "live smoke: requires ANTHROPIC_API_KEY, or set JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1 to use Claude OAuth credentials"]
async fn live_anthropic_reasoning_smoke() -> Result<()> {
    let _env_lock = jcode_base::storage::lock_test_env();
    let using_api_key = std::env::var_os("ANTHROPIC_API_KEY").is_some();
    let allow_oauth = std::env::var_os("JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH").is_some();
    if !using_api_key && !allow_oauth {
        eprintln!(
            "skipping live Anthropic smoke: set ANTHROPIC_API_KEY or JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1"
        );
        return Ok(());
    }

    let _max_tokens = EnvVarGuard::set_if_missing("JCODE_ANTHROPIC_MAX_TOKENS", "2048");
    let model = std::env::var("JCODE_LIVE_ANTHROPIC_MODEL")
        .or_else(|_| std::env::var("JCODE_ANTHROPIC_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
    let effort = std::env::var("JCODE_LIVE_ANTHROPIC_REASONING_EFFORT")
        .unwrap_or_else(|_| "low".to_string());
    let prompt = std::env::var("JCODE_LIVE_ANTHROPIC_PROMPT")
        .unwrap_or_else(|_| "Live smoke test: answer exactly OK.".to_string());
    let system = std::env::var("JCODE_LIVE_ANTHROPIC_SYSTEM").unwrap_or_else(|_| {
        "You are a live provider smoke test. Keep the answer tiny.".to_string()
    });
    let require_thinking = std::env::var_os("JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING").is_some();

    let provider = AnthropicProvider::new();
    provider.set_model(&model)?;
    // Some models (e.g. Fable 5) legitimately reject any reasoning effort. Treat
    // that as "use the model default" so the live call still exercises the model
    // rather than aborting the smoke test before any request is sent.
    if let Err(err) = provider.set_reasoning_effort(&effort) {
        eprintln!(
            "model {model} does not support reasoning effort '{effort}' ({err}); using model default"
        );
    }

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: prompt,
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let stream = provider.complete(&messages, &[], &system, None).await?;
    let (text_bytes, thinking_bytes, saw_message_end) =
        collect_live_smoke_stream(stream, std::time::Duration::from_secs(90)).await?;

    eprintln!(
        "live Anthropic reasoning smoke passed: model={model}, effort={effort}, text_bytes={text_bytes}, thinking_bytes={thinking_bytes}, message_end={saw_message_end}"
    );
    assert!(
        text_bytes > 0 || thinking_bytes > 0,
        "live Anthropic response contained neither text nor thinking deltas"
    );
    if require_thinking {
        assert!(
            thinking_bytes > 0,
            "live Anthropic response did not include thinking deltas despite JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_dangling_tool_use_repair() {
    let provider = AnthropicProvider::new();

    // Create messages with a dangling tool_use (no corresponding tool_result)
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Let me check".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_123".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_456".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "/tmp/test"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        // Missing tool_results for tool_123 and tool_456!
    ];

    let formatted = provider.format_messages(&messages);

    // Should have 3 messages:
    // 1. User: "Hello"
    // 2. Assistant: text + tool_uses
    // 3. User: synthetic tool_results for the dangling tool_uses
    assert_eq!(formatted.len(), 3);

    // Check the synthetic tool_result message
    let synthetic_msg = &formatted[2];
    assert_eq!(synthetic_msg.role, "user");
    assert_eq!(synthetic_msg.content.len(), 2);

    // Verify both tool_results are present
    let mut found_ids = std::collections::HashSet::new();
    for block in &synthetic_msg.content {
        if let ApiContentBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
            ..
        } = block
        {
            found_ids.insert(tool_use_id.clone());
            assert!(is_error);
            match content {
                ToolResultContent::Text(t) => assert!(t.contains("interrupted")),
                ToolResultContent::Blocks(_) => panic!("Expected text content"),
            }
        } else {
            panic!("Expected ToolResult block");
        }
    }
    assert!(found_ids.contains("tool_123"));
    assert!(found_ids.contains("tool_456"));
}

#[tokio::test]
async fn test_no_repair_when_tool_results_present() {
    let provider = AnthropicProvider::new();

    // Create messages where tool_use has a corresponding tool_result
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_123".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_123".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages);

    // Should have exactly 3 messages (no synthetic ones added)
    assert_eq!(formatted.len(), 3);

    // The last message should be the actual tool_result, not synthetic
    let last_msg = &formatted[2];
    if let ApiContentBlock::ToolResult { content, .. } = &last_msg.content[0] {
        match content {
            ToolResultContent::Text(t) => assert!(t.contains("file1.txt")),
            ToolResultContent::Blocks(_) => panic!("Expected text content"),
        }
    } else {
        panic!("Expected ToolResult block");
    }
}

#[tokio::test]
async fn test_parallel_image_tool_results_stay_contiguous() {
    // Regression for Anthropic 400: "`tool_use` ids were found without `tool_result`
    // blocks immediately after". When the assistant issues several parallel `read`
    // calls that return images, each tool result is stored as its own user message in
    // the form [tool_result, image, "[Attached image ...]" text]. After merging the
    // consecutive user messages, the sibling label text blocks were wedged between the
    // tool_results, which Anthropic rejects. The label must be folded into the
    // tool_result content so every tool_result stays contiguous.
    let provider = AnthropicProvider::new();

    let make_image_result = |id: &str, label: &str| Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: format!("Image: {label}"),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
            ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {label}]"
                ),
                cache_control: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    };

    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "tool_a".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "a.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "b.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_c".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "c.png"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_image_result("tool_a", "a.png"),
        make_image_result("tool_b", "b.png"),
        make_image_result("tool_c", "c.png"),
    ];

    let formatted = provider.format_messages(&messages);

    // assistant message + merged user tool_result message
    assert_eq!(formatted.len(), 2);
    let user_msg = &formatted[1];
    assert_eq!(user_msg.role, "user");

    // Every block in the user message must be a tool_result (no sibling text blocks
    // wedged between them), and all three tool_use ids must be present.
    let mut seen = std::collections::HashSet::new();
    for block in &user_msg.content {
        match block {
            ApiContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                seen.insert(tool_use_id.clone());
                // Each image tool_result should carry its image and folded label text.
                match content {
                    ToolResultContent::Blocks(blocks) => {
                        assert!(
                            blocks
                                .iter()
                                .any(|b| matches!(b, ToolResultContentBlock::Image { .. })),
                            "image tool_result should contain an image block"
                        );
                        assert!(
                            blocks.iter().any(|b| matches!(
                                b,
                                ToolResultContentBlock::Text { text }
                                    if text.contains("[Attached image associated")
                            )),
                            "label text should be folded into the tool_result content"
                        );
                    }
                    ToolResultContent::Text(_) => {
                        panic!("image tool_result should use block content")
                    }
                }
            }
            _ => panic!("expected only tool_result blocks in the user message"),
        }
    }
    assert_eq!(
        seen,
        ["tool_a", "tool_b", "tool_c"]
            .iter()
            .map(|s| s.to_string())
            .collect::<std::collections::HashSet<_>>()
    );
}

#[test]
fn oauth_system_is_the_identity_blocks_plus_the_static_prompt() {
    let Some(ApiSystem::Blocks(blocks)) = build_system_param("static prompt", true) else {
        panic!("Expected Blocks variant");
    };
    // Billing header and SDK identity, then the static prompt. Nothing
    // per-request is ever added (INT-01 R08). The request's cache placement
    // marks the last block (see `production_requests_read_where_the_previous_request_wrote`).
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[2].text, "static prompt");
    assert!(blocks.iter().all(|block| block.cache_control.is_none()));
}

#[test]
fn api_key_system_is_only_the_static_prompt() {
    let Some(ApiSystem::Blocks(blocks)) = build_system_param("static prompt", false) else {
        panic!("Expected Blocks variant");
    };
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "static prompt");
    assert!(build_system_param("", false).is_none());
}

#[tokio::test]
async fn test_sanitize_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "chatcmpl-BF2xX.tool_call.0".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "chatcmpl-BF2xX.tool_call.0".to_string(),
                content: "file1.txt".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages);

    let sanitized_id = "chatcmpl-BF2xX_tool_call_0";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn test_sanitize_dangling_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call.with.dots".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "crash"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages);

    let sanitized_id = "call_with_dots";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

/// The runtime-provider identity that `set_credential_mode` writes must decode
/// back to the exact same credential mode. This guards the model picker / header
/// widget from reporting OAuth when an API key is in use (or vice versa): the
/// env key is the single source of truth those surfaces read, so an asymmetric
/// mapping here would surface an inaccurate auth method to the user.
#[test]
fn credential_mode_runtime_provider_identity_round_trips() {
    let _guard = jcode_base::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_RUNTIME_PROVIDER");

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::OAuth,
        "OAuth selection must surface as the OAuth runtime identity"
    );

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude-api");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::ApiKey,
        "API-key selection must surface as the API-key runtime identity"
    );

    match previous {
        Some(value) => jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", value),
        None => jcode_base::env::remove_var("JCODE_RUNTIME_PROVIDER"),
    }
}

#[tokio::test]
async fn auto_mode_falls_back_to_api_key_when_oauth_is_expired() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "auto");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::Auto
    );

    let (token, is_oauth) = provider.get_access_token().await.unwrap();
    assert_eq!(token, "test-anthropic-api-key");
    assert!(
        !is_oauth,
        "automatic fallback must use API-key request semantics"
    );
}

#[tokio::test]
async fn explicit_oauth_mode_does_not_silently_fall_back_to_api_key() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "claude");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::OAuth
    );

    let error = provider.get_access_token().await.unwrap_err().to_string();
    assert!(error.contains("expired"), "unexpected error: {error}");
}

#[test]
fn test_anthropic_fable_5_sends_reasoning_fields() {
    // `claude-fable-5` rejected reasoning fields during its preview, but the
    // released model accepts an adaptive `thinking` block and an
    // `output_config` effort (verified live 2026-07-01). The request builder
    // must send both when an effort is configured.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("high".to_string());

    let (thinking, output_config, temperature) = provider.build_reasoning_request_parts_inner(
        "claude-fable-5",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert!(
        matches!(thinking, Some(ApiThinking::Adaptive { .. })),
        "Fable 5 should send an adaptive thinking block"
    );
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("high"),
        "Fable 5 should send the configured output_config effort"
    );
    assert_eq!(temperature, None);

    // Fable 5 supports the real `max` API level, so `max` is sent verbatim.
    *provider.reasoning_effort.write().unwrap() = Some("max".to_string());
    let (_thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-fable-5",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("max")
    );

    // The effort picker surfaces levels for Fable 5.
    assert!(AnthropicProvider::model_supports_reasoning_effort(
        "claude-fable-5"
    ));
}

#[test]
fn detects_anthropic_reasoning_unsupported_errors() {
    // The real 400 bodies returned when Fable 5 is sent reasoning fields.
    let thinking_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"adaptive thinking is not supported on this model\"}}";
    assert!(is_reasoning_unsupported_error(thinking_400));
    let effort_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"this model does not support the effort parameter.\"}}";
    assert!(is_reasoning_unsupported_error(effort_400));

    // Unrelated 400s must not trigger the reasoning self-heal path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (400 bad request): {\"type\":\"invalid_request_error\",\"message\":\"max_tokens too large\"}"
    ));
    // A thinking-mentioning error that is not a 400 must not match either.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (429 too many requests): rate_limit on thinking budget"
    ));
    // Model-not-found is a different recovery path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (404 not found): {\"type\":\"not_found_error\",\"message\":\"model not found\"}"
    ));
}

#[test]
fn detects_anthropic_model_not_found_errors() {
    // The real 404 body returned when a model id was retired (e.g. Fable 5).
    let real = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8.\"}}";
    assert!(is_model_not_found_error(real));

    // Structural marker alone (lowercased error chain).
    assert!(is_model_not_found_error(
        "model claude-foo not found (not_found_error)"
    ));

    // Unrelated failures must not trigger the model fallback path.
    assert!(!is_model_not_found_error(
        "anthropic api error (401 unauthorized): invalid authentication credentials"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (429 too many requests): rate_limit"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (404 not found): resource missing"
    ));
}

#[test]
fn anthropic_fallback_prefers_best_available_and_skips_tried_and_retired() {
    // The fallback logic reads the process-global model catalog; lock and
    // reset it so fixture models hydrated by other tests cannot leak in.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    let known = jcode_base::provider::known_anthropic_model_ids();
    assert!(
        !known.is_empty(),
        "expected a non-empty Anthropic model catalog"
    );

    // With nothing tried, the fallback offers the highest-quality (flagship)
    // model, NOT merely the first catalog entry. The curated order ranks Opus
    // ahead of Haiku, so the chosen model must not be a Haiku/retired tier when
    // a stronger one exists.
    let first = anthropic_fallback_model(&[], "").expect("a fallback should exist");
    let first_key = AnthropicProvider::normalized_model_key(&first);
    assert!(
        !first_key.contains("haiku"),
        "fallback must not downgrade to Haiku when a flagship is available, got {first}"
    );
    assert!(
        !anthropic_model_is_retired(&first),
        "fallback must never pick a retired model, got {first}"
    );

    // A retired model in `tried` must never be re-offered, and the result must
    // skip retired families entirely.
    let next = anthropic_fallback_model(&["claude-mythos-1".to_string()], "")
        .expect("another fallback should exist");
    assert!(!anthropic_model_is_retired(&next));

    // Exhausting every viable known model yields None.
    let exhausted = anthropic_fallback_model(&known, "");
    assert!(
        exhausted.is_none(),
        "no fallback should remain once all known models are tried, got {exhausted:?}"
    );
}

#[test]
fn anthropic_fallback_honors_server_recommendation() {
    // The recommendation matcher scores hints against the process-global model
    // catalog; lock and reset it so fixture models hydrated by other tests
    // (e.g. claude-opus-5-preview) cannot outrank the real catalog entries.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    // The real 404 body recommends a specific replacement model. We must honor
    // it over the generic quality ranking.
    let body = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8. learn more: https://anthropic.com\"}}";
    let recommended =
        anthropic_recommended_model_from_error(body).expect("should parse a recommendation");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&recommended),
        "claude-opus-4-8",
        "server recommendation 'Opus 4.8' should map to claude-opus-4-8"
    );

    // The full fallback also returns the recommended model.
    let fallback = anthropic_fallback_model(&["claude-mythos-1".to_string()], body)
        .expect("a fallback should exist");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&fallback),
        "claude-opus-4-8"
    );

    // A recommendation pointing at a retired model is ignored (falls through to
    // quality ranking).
    let retired_rec = "model x not available. please use mythos 1.";
    assert!(
        anthropic_recommended_model_from_error(retired_rec).is_none()
            || !anthropic_model_is_retired(
                &anthropic_recommended_model_from_error(retired_rec).unwrap()
            )
    );

    // No recommendation phrase -> None.
    assert!(anthropic_recommended_model_from_error("429 too many requests").is_none());
}

#[test]
fn anthropic_quality_rank_orders_opus_before_haiku_and_retired_last() {
    let opus = anthropic_model_quality_rank("claude-opus-4-8");
    let sonnet = anthropic_model_quality_rank("claude-sonnet-4-6");
    let haiku = anthropic_model_quality_rank("claude-haiku-4-5");
    let retired = anthropic_model_quality_rank("claude-mythos-1");
    // Fable 5 is live again and curated as the flagship, so it ranks first.
    let fable = anthropic_model_quality_rank("claude-fable-5");
    assert!(
        fable <= opus,
        "Fable 5 should rank at or ahead of Opus ({fable} vs {opus})"
    );
    assert!(
        opus < sonnet,
        "Opus should outrank Sonnet ({opus} vs {sonnet})"
    );
    assert!(
        sonnet < haiku,
        "Sonnet should outrank Haiku ({sonnet} vs {haiku})"
    );
    assert!(
        haiku < retired,
        "retired models must sort last ({haiku} vs {retired})"
    );
    assert_eq!(retired, usize::MAX);
    // Dated live ids must rank like their canonical base.
    assert_eq!(
        anthropic_model_quality_rank("claude-haiku-4-5-20251001"),
        haiku
    );
}

#[test]
fn fable_quota_fallback_selects_the_best_available_opus() {
    let fallback = AnthropicProvider::best_available_opus_model("claude-fable-5")
        .expect("the curated Anthropic catalog should contain an Opus fallback");
    assert!(
        fallback.contains("claude-opus"),
        "unexpected fallback: {fallback}"
    );

    let candidates = jcode_base::provider::cached_anthropic_model_ids()
        .unwrap_or_else(jcode_base::provider::known_anthropic_model_ids);
    let best_rank = candidates
        .iter()
        .filter(|model| model.to_ascii_lowercase().contains("claude-opus"))
        .filter(|model| !anthropic_model_is_retired(model))
        .map(|model| anthropic_model_quality_rank(model))
        .min()
        .expect("available Opus model");
    assert_eq!(anthropic_model_quality_rank(&fallback), best_rank);
}

#[test]
fn model_scoped_usage_routes_only_exhausted_fable_to_opus() {
    let usage = jcode_base::usage::UsageData {
        model_scoped: vec![jcode_base::usage::ModelScopedUsageWindow {
            model_name: "Fable".to_string(),
            utilization: 1.0,
            resets_at: Some("2026-08-11T00:00:00Z".to_string()),
        }],
        ..Default::default()
    };
    let fallback = AnthropicProvider::fallback_for_model_scoped_usage("claude-fable-5", &usage)
        .expect("exhausted Fable should route to Opus");
    assert!(
        fallback.contains("claude-opus"),
        "unexpected fallback: {fallback}"
    );
    assert!(
        AnthropicProvider::fallback_for_model_scoped_usage("claude-opus-5", &usage).is_none(),
        "an exhausted Fable scope must not reroute an explicitly selected Opus"
    );

    let available = jcode_base::usage::UsageData {
        model_scoped: vec![jcode_base::usage::ModelScopedUsageWindow {
            model_name: "Fable".to_string(),
            utilization: 0.98,
            resets_at: None,
        }],
        ..Default::default()
    };
    assert!(
        AnthropicProvider::fallback_for_model_scoped_usage("claude-fable-5", &available).is_none(),
        "Fable must remain selected while its scoped quota is available"
    );
}

#[test]
fn detects_live_fable_scoped_limit_errors_without_misrouting_other_limits() {
    assert!(is_fable_scoped_limit_error(
        "claude-fable-5",
        r#"429 {"type":"rate_limit_error","message":"You have reached your weekly Fable limit"}"#,
    ));
    assert!(is_fable_scoped_limit_error(
        "claude-fable-5",
        "usage limit reached for the 7-day model window",
    ));
    assert!(!is_fable_scoped_limit_error(
        "claude-opus-5",
        "weekly Fable rate limit reached",
    ));
    assert!(!is_fable_scoped_limit_error(
        "claude-fable-5",
        "429 overloaded_error: service temporarily overloaded",
    ));
    assert!(!is_fable_scoped_limit_error(
        "claude-fable-5",
        "global 5-hour rate limit reached",
    ));
}

#[test]
fn ping_keepalive_emits_streaming_phase_event() {
    // Issue #451: during silent reasoning phases, `ping` events can be the
    // only upstream traffic. They must surface as a StreamEvent so the client
    // stall guard sees activity instead of cancelling a healthy stream.
    let mut state = test_sse_state();
    let event = SseEvent {
        event_type: "ping".to_string(),
        data: r#"{"type": "ping"}"#.to_string(),
    };
    let events = process_sse_event(&event, &mut state);
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ConnectionPhase {
                phase: jcode_message_types::ConnectionPhase::Streaming
            }
        )),
        "expected ping to emit a Streaming ConnectionPhase event, got {events:?}"
    );
}

#[test]
fn test_anthropic_opus_5_low_effort_reaches_the_wire() {
    // Benchmark campaigns pin `claude-opus-5` at `low` effort. Opus 5 also
    // *defaults* to `low` (jcode's default model/effort pairing), and an
    // explicit `low` must survive normalization, must NOT be silently
    // promoted, and must land in `output_config.effort` on the request.
    assert!(jcode_provider_core::anthropic_reasoning_caps("claude-opus-5").output_effort);
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-5").as_deref(),
        Some("low"),
    );
    assert_eq!(
        AnthropicProvider::normalize_reasoning_effort("low").as_deref(),
        Some("low"),
    );
    // Downward selection is never clamped upward toward the model default.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-5", "low"),
        "low",
    );
    assert_eq!(
        AnthropicProvider::store_effort_for_model("claude-opus-5", "low"),
        "low",
    );

    let provider = AnthropicProvider::new();
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-5".to_string();
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));

    let (thinking, output_config, _temp) = provider.build_reasoning_request_parts_inner(
        "claude-opus-5",
        true,
        false,
        PrefixMismatchBehavior::Error,
    );
    assert_eq!(
        output_config
            .expect("explicit low effort should set output_config")
            .effort,
        "low",
    );
    // Opus 5 rejects `thinking.type.enabled`; it requires adaptive thinking.
    assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
}

/// A `content_block_start` carrying an unrecognized block type must still
/// deserialize. Before the `Unknown` catch-all the whole event failed to parse
/// and was dropped, so an unknown *tool* block produced a turn that reported
/// `stop_reason: tool_use` with no tool call for the agent to run.
#[test]
fn test_anthropic_unknown_content_block_start_does_not_drop_event() {
    for block_type in [
        "server_tool_use",
        "web_search_tool_result",
        "some_future_block",
    ] {
        let mut state = test_sse_state();
        let event = SseEvent {
            event_type: "content_block_start".to_string(),
            data: serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": block_type, "id": "srvtoolu_1", "name": "web_search"}
            })
            .to_string(),
        };
        let events = process_sse_event(&event, &mut state);
        assert!(
            events.is_empty(),
            "{block_type}: unknown block must not synthesize stream events"
        );
        assert!(
            state.current_tool_use.is_none(),
            "{block_type}: unknown block must not start tool accumulation"
        );
        assert!(
            state.current_thinking.is_none(),
            "{block_type}: unknown block must not start a thinking block"
        );
    }
}

fn binding_behavior(thinking: &Option<ApiThinking>) -> Option<PrefixMismatchBehavior> {
    thinking
        .as_ref()
        .and_then(ApiThinking::block_binding)
        .map(|binding| binding.prefix_mismatch_behavior)
}

#[test]
fn prefix_bound_models_carry_the_binding_control_with_the_requested_behavior() {
    let provider = AnthropicProvider::new();
    for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"] {
        for behavior in [
            PrefixMismatchBehavior::Error,
            PrefixMismatchBehavior::DropBlock,
        ] {
            let (thinking, _, temperature) =
                provider.build_reasoning_request_parts_inner(model, true, true, behavior);
            assert_eq!(binding_behavior(&thinking), Some(behavior), "{model}");
            assert!(temperature.is_none(), "{model}: thinking is present");
        }
    }
    let wire = serde_json::to_value(with_binding_control(
        Some(ApiThinking::Adaptive {
            display: Some("summarized"),
            block_binding: None,
        }),
        "claude-opus-5-5",
        PrefixMismatchBehavior::DropBlock,
    ))
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({
            "type": "adaptive",
            "display": "summarized",
            "block_binding": {"prefix_mismatch_behavior": "drop_block"}
        })
    );
}

#[test]
fn a_prefix_bound_request_without_thinking_gains_adaptive_thinking_for_the_control() {
    let thinking = with_binding_control(None, "claude-opus-5-5", PrefixMismatchBehavior::Error);
    assert_eq!(
        serde_json::to_value(&thinking).unwrap(),
        serde_json::json!({"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "error"}}),
        "no display is requested, so the model's default display is kept"
    );
}

#[test]
fn unbound_models_send_no_binding_control() {
    let provider = AnthropicProvider::new();
    for model in [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-opus-4-8",
    ] {
        let (thinking, _, _) = provider.build_reasoning_request_parts_inner(
            model,
            true,
            true,
            PrefixMismatchBehavior::Error,
        );
        assert_eq!(binding_behavior(&thinking), None, "{model}");
        assert!(
            with_binding_control(None, model, PrefixMismatchBehavior::Error).is_none(),
            "{model}"
        );
    }
}

#[test]
fn unit_tests_default_to_error_and_the_environment_can_choose() {
    let _lock = jcode_base::storage::lock_test_env();
    let guard = EnvVarGuard::set(PREFIX_MISMATCH_ENV, "drop_block");
    assert_eq!(
        prefix_mismatch_behavior(),
        PrefixMismatchBehavior::DropBlock
    );
    drop(guard);
    let guard = EnvVarGuard::set(PREFIX_MISMATCH_ENV, "error");
    assert_eq!(prefix_mismatch_behavior(), PrefixMismatchBehavior::Error);
    drop(guard);
    let guard = EnvVarGuard::set(PREFIX_MISMATCH_ENV, "sometimes");
    assert_eq!(prefix_mismatch_behavior(), PrefixMismatchBehavior::Error);
    drop(guard);
}

#[test]
fn the_beta_header_follows_the_request_thinking_parameter() {
    let base = "prompt-caching-2024-07-31";
    assert_eq!(anthropic_beta_header(base, ThinkingBetas::default()), base);
    let bound = ThinkingBetas {
        thinking: true,
        binding_controls: true,
    };
    let header = anthropic_beta_header(base, bound);
    assert_eq!(
        header,
        "prompt-caching-2024-07-31,interleaved-thinking-2025-05-14,thinking-binding-controls-2026-08-01"
    );
    assert_eq!(
        anthropic_beta_header(&header, bound),
        header,
        "betas already present are not repeated"
    );
    let body = serde_json::json!({"thinking": {"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "error"}}});
    assert_eq!(ThinkingBetas::of_body(&body), bound);
    let mut request = serde_json::from_value::<serde_json::Value>(body).unwrap();
    request["thinking"]
        .as_object_mut()
        .unwrap()
        .remove("block_binding");
    assert_eq!(
        ThinkingBetas::of_body(&request),
        ThinkingBetas {
            thinking: true,
            binding_controls: false
        }
    );
}

#[test]
fn the_anthropic_runtime_replays_signed_thinking() {
    assert_eq!(
        AnthropicProvider::new().reasoning_replay_kind(),
        Some(jcode_provider_core::ContextReasoningBlockKind::AnthropicThinking)
    );
}

// Recorded Claude OAuth streams (see `fixtures/sse/README.md`).
const FIXTURE_SUMMARIZED: &str =
    include_str!("../fixtures/sse/claude-opus-5-5-thinking-summarized.sse");
const FIXTURE_OMITTED: &str = include_str!("../fixtures/sse/claude-opus-5-5-thinking-omitted.sse");
const FIXTURE_FOLLOW_UP: &str =
    include_str!("../fixtures/sse/claude-opus-5-5-thinking-follow-up.sse");

/// The assistant content a raw stream carries, rebuilt independently of the
/// runtime by concatenating each block's deltas, in wire form.
fn wire_content_from_sse(sse: &str) -> Vec<serde_json::Value> {
    use serde_json::{Value, json};
    let mut blocks: Vec<Value> = Vec::new();
    let mut inputs: Vec<String> = Vec::new();
    for line in sse.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let event: Value = serde_json::from_str(data.trim()).expect("fixture event is JSON");
        let index = event["index"].as_u64().unwrap_or_default() as usize;
        match event["type"].as_str() {
            Some("content_block_start") => {
                let start = &event["content_block"];
                let block = match start["type"].as_str() {
                    Some("thinking") => {
                        json!({"type": "thinking", "thinking": start["thinking"], "signature": start["signature"]})
                    }
                    Some("redacted_thinking") => {
                        json!({"type": "redacted_thinking", "data": start["data"]})
                    }
                    Some("text") => json!({"type": "text", "text": start["text"]}),
                    Some("tool_use") => {
                        json!({"type": "tool_use", "id": start["id"], "name": start["name"]})
                    }
                    other => panic!("unexpected block {other:?}"),
                };
                blocks.resize(index + 1, Value::Null);
                inputs.resize(index + 1, String::new());
                blocks[index] = block;
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let (field, text) = match delta["type"].as_str() {
                    Some("thinking_delta") => ("thinking", &delta["thinking"]),
                    Some("signature_delta") => ("signature", &delta["signature"]),
                    Some("text_delta") => ("text", &delta["text"]),
                    Some("input_json_delta") => {
                        inputs[index].push_str(delta["partial_json"].as_str().unwrap());
                        continue;
                    }
                    other => panic!("unexpected delta {other:?}"),
                };
                let current = blocks[index][field]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                blocks[index][field] = json!(current + text.as_str().unwrap());
            }
            Some("content_block_stop") if blocks[index]["type"] == "tool_use" => {
                blocks[index]["input"] = serde_json::from_str(&inputs[index]).unwrap();
            }
            _ => {}
        }
    }
    blocks
}

/// Run a raw stream through the production path: the SSE parser, the shared
/// assembler, a persistence round trip, and the Anthropic formatter.
fn replay_through_production_path(
    sse: &str,
    request_binding: RequestBinding,
) -> (Vec<ContentBlock>, Vec<serde_json::Value>) {
    use jcode_base::message::AssistantTurnAssembler;
    use jcode_message_types::ToolCall;

    let mut state = SseStreamState::new("claude-opus-5-5", request_binding);
    let mut buffer = format!("{sse}\n\n");
    let mut turn = AssistantTurnAssembler::new();
    let mut tools: Vec<ToolCall> = Vec::new();
    let mut current: Option<(ToolCall, String)> = None;
    while let Some(event) = parse_sse_event(&mut buffer) {
        for event in process_sse_event(&event, &mut state) {
            match event {
                StreamEvent::ThinkingStart => turn.reasoning_started(),
                StreamEvent::ThinkingDelta(text) => turn.reasoning_delta(&text),
                StreamEvent::ThinkingEnd => turn.reasoning_ended(),
                StreamEvent::ReplayableReasoning(block) => turn.replayable_reasoning(block),
                StreamEvent::TextDelta(text) => turn.text(&text),
                StreamEvent::ToolUseStart { id, name } => {
                    turn.tool_use_started(&id);
                    current = Some((
                        ToolCall {
                            id,
                            name,
                            ..ToolCall::default()
                        },
                        String::new(),
                    ));
                }
                StreamEvent::ToolInputDelta(delta) => {
                    current.as_mut().unwrap().1.push_str(&delta);
                }
                StreamEvent::ToolUseEnd => {
                    let (mut call, input) = current.take().unwrap();
                    call.input = ToolCall::parse_streamed_input_to_object(&input);
                    tools.push(call);
                }
                _ => {}
            }
        }
    }
    let stored = turn.content_blocks(
        Some(jcode_provider_core::ContextReasoningBlockKind::AnthropicThinking),
        &tools,
    );
    let persisted = serde_json::to_string(&stored).unwrap();
    let reloaded: Vec<ContentBlock> = serde_json::from_str(&persisted).unwrap();
    let wire = serde_json::to_value(jcode_provider_anthropic::format_content_blocks(&reloaded))
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    (reloaded, wire)
}

#[test]
fn recorded_streams_replay_byte_exact_through_capture_storage_and_formatting() {
    for (name, sse) in [
        ("summarized", FIXTURE_SUMMARIZED),
        ("omitted", FIXTURE_OMITTED),
        ("follow-up", FIXTURE_FOLLOW_UP),
    ] {
        let (_, wire) = replay_through_production_path(sse, test_request_binding(None));
        assert_eq!(wire, wire_content_from_sse(sse), "{name}");
    }
    // The omitted-display recording is the empty-text signed block case.
    let (stored, _) = replay_through_production_path(FIXTURE_OMITTED, test_request_binding(None));
    assert!(stored.iter().any(|block| matches!(
        block,
        ContentBlock::AnthropicThinking { thinking, signature, .. }
            if thinking.is_empty() && !signature.is_empty()
    )));
}

#[test]
fn a_recorded_second_turn_chains_to_the_first_and_both_replay_as_valid() {
    use jcode_provider_anthropic::binding::{ReplayValidity, analyze_request};

    let task = Message::user("task");
    let first_request = ApiRequest {
        model: "claude-opus-5-5".to_string(),
        max_tokens: 1024,
        system: build_system_param("probe system", true),
        messages: jcode_provider_anthropic::format_messages(std::slice::from_ref(&task)),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    };
    let first_binding = analyze_request(&first_request).binding;
    let (first_turn, first_wire) =
        replay_through_production_path(FIXTURE_SUMMARIZED, first_binding.clone());
    let first_tool = first_wire
        .iter()
        .find(|block| block["type"] == "tool_use")
        .and_then(|block| block["id"].as_str())
        .unwrap()
        .to_string();

    let mut history = vec![
        task,
        Message {
            role: Role::Assistant,
            content: first_turn,
            timestamp: None,
            tool_duration_ms: None,
        },
        Message::tool_result(&first_tool, "parity-probe", false),
    ];
    let second_request = ApiRequest {
        messages: jcode_provider_anthropic::format_messages(&history),
        ..first_request.clone()
    };
    let second_analysis = analyze_request(&second_request);
    assert_eq!(
        second_analysis
            .replayed
            .iter()
            .map(|block| block.validity)
            .collect::<Vec<_>>(),
        vec![ReplayValidity::Valid],
        "the first turn's thinking replays unchanged"
    );
    let (second_turn, _) =
        replay_through_production_path(FIXTURE_FOLLOW_UP, second_analysis.binding.clone());
    let second_binding = second_turn
        .iter()
        .find_map(ContentBlock::anthropic_thinking_binding)
        .unwrap()
        .clone();
    assert_eq!(
        second_binding.predecessor,
        second_analysis.binding.last_thinking
    );
    assert!(second_binding.predecessor.is_some());

    history.push(Message {
        role: Role::Assistant,
        content: second_turn,
        timestamp: None,
        tool_duration_ms: None,
    });
    history.push(Message::user("next"));
    let both = ApiRequest {
        messages: jcode_provider_anthropic::format_messages(&history),
        ..first_request.clone()
    };
    assert!(
        analyze_request(&both)
            .replayed
            .iter()
            .all(|block| block.validity == ReplayValidity::Valid)
    );

    // Suppressing the first turn's thinking (a leading run) keeps the second
    // turn's thinking valid; T8 checks the same rule live.
    history[1]
        .content
        .retain(|block| !matches!(block, ContentBlock::AnthropicThinking { .. }));
    let stripped = ApiRequest {
        messages: jcode_provider_anthropic::format_messages(&history),
        ..first_request
    };
    let replayed = analyze_request(&stripped).replayed;
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].validity, ReplayValidity::Valid);
}

/// Synthetic: the recorded models returned one thinking block per response
/// and never `redacted_thinking`, so this stream composes several blocks in
/// one response from the recorded payloads plus a fabricated redacted block.
fn synthetic_multi_block_stream() -> String {
    use serde_json::json;
    let summarized = wire_content_from_sse(FIXTURE_SUMMARIZED);
    let omitted = wire_content_from_sse(FIXTURE_OMITTED);
    let thinking = |block: &serde_json::Value| {
        (
            block["thinking"].as_str().unwrap().to_string(),
            block["signature"].as_str().unwrap().to_string(),
        )
    };
    let (first_text, first_signature) = thinking(&summarized[0]);
    let (_, empty_signature) = thinking(&omitted[0]);
    let mut events = vec![
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5", "input_transformations": []}}),
    ];
    let mut index = 0;
    let push_thinking = |events: &mut Vec<serde_json::Value>,
                         index: usize,
                         text: &str,
                         signature: &str| {
        events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}));
        for chunk in text.as_bytes().chunks(40) {
            events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": String::from_utf8_lossy(chunk)}}));
        }
        events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "signature_delta", "signature": signature}}));
        events.push(json!({"type": "content_block_stop", "index": index}));
    };
    push_thinking(&mut events, index, &first_text, &first_signature);
    index += 1;
    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}));
    events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": "Checking the arithmetic."}}));
    events.push(json!({"type": "content_block_stop", "index": index}));
    index += 1;
    push_thinking(&mut events, index, "", &empty_signature);
    index += 1;
    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "redacted_thinking", "data": "synthetic-redacted-payload=="}}));
    events.push(json!({"type": "content_block_stop", "index": index}));
    index += 1;
    events.push(json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": "toolu_synthetic", "name": "bash", "input": {}}}));
    events.push(json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"echo ok\"}"}}));
    events.push(json!({"type": "content_block_stop", "index": index}));
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));
    events
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {}\n\n",
                event["type"].as_str().unwrap(),
                event
            )
        })
        .collect()
}

#[test]
fn several_signed_blocks_interleaved_with_text_and_a_redacted_block_replay_in_order() {
    let sse = synthetic_multi_block_stream();
    let (stored, wire) = replay_through_production_path(&sse, test_request_binding(None));
    assert_eq!(wire, wire_content_from_sse(&sse));
    let kinds: Vec<&str> = wire
        .iter()
        .map(|block| block["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "thinking",
            "text",
            "thinking",
            "redacted_thinking",
            "tool_use"
        ]
    );
    // Each block chains to the one before it, in stream order.
    let bindings: Vec<_> = stored
        .iter()
        .filter_map(ContentBlock::anthropic_thinking_binding)
        .collect();
    assert_eq!(bindings.len(), 3);
    assert_eq!(bindings[0].predecessor, None);
    assert!(bindings[1].predecessor.is_some() && bindings[2].predecessor.is_some());
    assert_ne!(bindings[1].predecessor, bindings[2].predecessor);
}

/// Bind one produced assistant turn the way the runtime binds it: to the
/// request `build_api_request` builds for the history before it.
/// Select a model without the catalog check `set_model` performs: the
/// catalog is process-global and other tests replace it.
fn use_model(provider: &AnthropicProvider, model: &str) {
    *provider.model.write().unwrap() = model.to_string();
}

fn produced_turn_for(
    provider: &AnthropicProvider,
    history: &[Message],
    tools: &[ToolDefinition],
    is_oauth: bool,
    signature: &str,
    tool_id: &str,
) -> Message {
    let model = provider.model();
    let request = provider.build_api_request(
        &model,
        history,
        tools,
        build_system_param("probe system", is_oauth),
        is_oauth,
    );
    let binding = jcode_provider_anthropic::binding::analyze_request(&request).binding;
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::AnthropicThinking {
                thinking: format!("thought {signature}"),
                signature: signature.to_string(),
                binding: Some(AnthropicThinkingBinding {
                    model: model.clone(),
                    prefix_digest: binding.prefix_digest,
                    predecessor: binding.last_thinking,
                }),
            },
            ContentBlock::ToolUse {
                id: tool_id.to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "true"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn probe_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "bash".to_string(),
        description: "Run a command".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }),
    }]
}

/// Three tool rounds on a prefix-bound model, each turn bound to the exact
/// production request that produced it.
fn bound_history(provider: &AnthropicProvider, is_oauth: bool) -> Vec<Message> {
    let tools = probe_tools();
    let mut history = vec![Message::user("task")];
    for (signature, tool_id) in [("sig-1", "t1"), ("sig-2", "t2"), ("sig-3", "t3")] {
        let turn = produced_turn_for(provider, &history, &tools, is_oauth, signature, tool_id);
        history.push(turn);
        history.push(Message::tool_result(tool_id, "ok", false));
    }
    history
}

#[test]
fn replayed_reasoning_invalidations_use_the_production_request_prefix() {
    use jcode_provider_core::{InvalidReplayedReasoning, ReplayedReasoningInvalidity};
    let provider = AnthropicProvider::new();
    use_model(&provider, "claude-sonnet-5-5");
    provider
        .last_request_route
        .store(ROUTE_API_KEY, Ordering::Relaxed);
    let tools = probe_tools();
    let history = bound_history(&provider, false);

    assert_eq!(
        provider.replayed_reasoning_invalidations(&history, &tools, "probe system"),
        Some(Vec::new()),
        "an append-only history keeps every block"
    );

    // A summary of the first round changes the history before the second and
    // third turns; the first turn's block precedes the edit.
    let mut summarized = history.clone();
    summarized[2] = Message::tool_result("t1", "ok (summarized)", false);
    assert_eq!(
        provider.replayed_reasoning_invalidations(&summarized, &tools, "probe system"),
        Some(vec![
            InvalidReplayedReasoning {
                message_index: 3,
                block_index: 0,
                invalidity: ReplayedReasoningInvalidity::PrefixChanged,
            },
            InvalidReplayedReasoning {
                message_index: 5,
                block_index: 0,
                invalidity: ReplayedReasoningInvalidity::PrefixChanged,
            },
        ])
    );

    // Suppressing the middle block breaks the third block's chain.
    let mut middle_removed = history.clone();
    middle_removed[3]
        .content
        .retain(|block| !matches!(block, ContentBlock::AnthropicThinking { .. }));
    assert_eq!(
        provider.replayed_reasoning_invalidations(&middle_removed, &tools, "probe system"),
        Some(vec![InvalidReplayedReasoning {
            message_index: 5,
            block_index: 0,
            invalidity: ReplayedReasoningInvalidity::ChainBroken,
        }])
    );

    // A changed static prompt or tool set invalidates every block.
    assert_eq!(
        provider
            .replayed_reasoning_invalidations(&history, &tools, "probe system + skill")
            .map(|blocks| blocks.len()),
        Some(3)
    );
    assert_eq!(
        provider
            .replayed_reasoning_invalidations(&history, &[], "probe system")
            .map(|blocks| blocks.len()),
        Some(3)
    );
}

#[test]
fn replayed_reasoning_invalidations_follow_the_credential_route() {
    let provider = AnthropicProvider::new();
    use_model(&provider, "claude-opus-5-5");
    let tools = probe_tools();
    provider
        .last_request_route
        .store(ROUTE_OAUTH, Ordering::Relaxed);
    let history = bound_history(&provider, true);
    assert_eq!(
        provider.replayed_reasoning_invalidations(&history, &tools, "probe system"),
        Some(Vec::new())
    );
    // The OAuth identity blocks are part of the bound prefix, so the same
    // history on the API-key route matches nothing.
    provider
        .last_request_route
        .store(ROUTE_API_KEY, Ordering::Relaxed);
    assert_eq!(
        provider
            .replayed_reasoning_invalidations(&history, &tools, "probe system")
            .map(|blocks| blocks.len()),
        Some(3)
    );
}

#[test]
fn unbound_models_report_no_replayed_reasoning_invalidation() {
    let provider = AnthropicProvider::new();
    use_model(&provider, "claude-sonnet-5-5");
    provider
        .last_request_route
        .store(ROUTE_API_KEY, Ordering::Relaxed);
    let tools = probe_tools();
    let mut history = bound_history(&provider, false);
    history[2] = Message::tool_result("t1", "edited", false);
    use_model(&provider, "claude-opus-5");
    assert_eq!(
        provider.replayed_reasoning_invalidations(&history, &tools, "probe system"),
        None,
        "Opus 5 does not bind thinking to its prefix (D11)"
    );
}

/// Marked message blocks of a request, as `(message, block)`.
fn marked_message_blocks(request: &ApiRequest) -> Vec<(usize, usize)> {
    request
        .messages
        .iter()
        .enumerate()
        .flat_map(|(message, m)| {
            m.content
                .iter()
                .enumerate()
                .filter(|(_, block)| block.has_cache_control())
                .map(move |(block, _)| (message, block))
        })
        .collect()
}

/// The request through `(message, block)`, cache markers removed.
fn cached_span(request: &ApiRequest, message: usize, block: usize) -> serde_json::Value {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.remove("cache_control");
                map.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut span = request.clone();
    span.messages.truncate(message + 1);
    span.messages[message].content.truncate(block + 1);
    let mut value = serde_json::json!({
        "tools": span.tools,
        "system": span.system,
        "messages": span.messages,
    });
    strip(&mut value);
    value
}

#[test]
fn production_requests_read_where_the_previous_request_wrote() {
    // INT-01 DESIGN §6, R12: a scripted Claude session through the production
    // builder, with thinking bound exactly as the runtime binds it.
    let provider = AnthropicProvider::new();
    use_model(&provider, "claude-opus-5-5");
    let tools = probe_tools();
    let mut history = vec![Message::user("start")];
    let mut requests = Vec::new();
    for step in 0..5 {
        let tool_id = format!("call_{step}");
        requests.push(provider.build_api_request(
            "claude-opus-5-5",
            &history,
            &tools,
            build_system_param("probe system", true),
            true,
        ));
        let turn = produced_turn_for(
            &provider,
            &history,
            &tools,
            true,
            &format!("sig-{step}"),
            &tool_id,
        );
        history.push(turn);
        history.push(Message::tool_result(&tool_id, "ok", false));
        if step == 2 {
            history.push(Message::user("a new user turn"));
        }
    }

    for request in &requests {
        let Some(ApiSystem::Blocks(blocks)) = &request.system else {
            panic!("system");
        };
        assert!(blocks.last().is_some_and(|b| b.cache_control.is_some()));
        assert!(
            request
                .tools
                .iter()
                .flatten()
                .all(|t| t.cache_control.is_none())
        );
        let markers = serde_json::to_string(request)
            .unwrap()
            .matches("cache_control")
            .count();
        assert!(markers <= 4, "at most four breakpoints, got {markers}");
        assert!(
            jcode_provider_anthropic::binding::analyze_request(request)
                .invalid()
                .next()
                .is_none(),
            "cache placement never changes what a replayed block is bound to"
        );
    }
    for pair in requests.windows(2) {
        let written = *marked_message_blocks(&pair[0]).last().expect("newest");
        assert!(marked_message_blocks(&pair[1]).contains(&written));
        assert_eq!(
            cached_span(&pair[0], written.0, written.1),
            cached_span(&pair[1], written.0, written.1),
            "the previous request's cached span is a byte-identical prefix"
        );
    }
}

#[test]
fn an_unconfigured_effort_is_the_model_default_not_none() {
    // The surfaced effort is persisted into the session and restored as an
    // explicit choice (`restore_reasoning_effort_from_session`). Reporting the
    // model default as `none` would turn thinking off on Sonnet 5 and lower
    // Sonnet 5.5 to `low` after a restore (INT-01 WP-05).
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;
    for model in ["claude-sonnet-5-5", "claude-sonnet-5", "claude-sonnet-4-6"] {
        use_model(&provider, model);
        let surfaced = provider.reasoning_effort();
        assert_eq!(surfaced, None, "{model}");
        let before = provider.build_reasoning_request_parts_inner(
            model,
            true,
            false,
            PrefixMismatchBehavior::DropBlock,
        );
        // Restoring the surfaced value (absent) leaves the request unchanged.
        provider.set_reasoning_effort("").unwrap();
        let after = provider.build_reasoning_request_parts_inner(
            model,
            true,
            false,
            PrefixMismatchBehavior::DropBlock,
        );
        assert_eq!(
            serde_json::to_value(&before.0).unwrap(),
            serde_json::to_value(&after.0).unwrap(),
            "{model}"
        );
        assert!(before.1.is_none() && after.1.is_none(), "{model}");
    }
    // A jcode default is surfaced as itself.
    use_model(&provider, "claude-opus-5-5");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("medium"));
}
