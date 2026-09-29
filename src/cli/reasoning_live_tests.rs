//! INT-01/WP-02 live acceptance for the path a Claude child runs.
//!
//! A delegated child's provider is resolved through the model roster to the
//! concrete Anthropic runtime, and the child runs the agent's streaming turn
//! loop. Only in-process file tools are used, because shell commands need the
//! server's native command worker. This test resolves the same way, runs a multi-tool task through that
//! loop against the real Claude OAuth route, and requires every request to be
//! accepted with `prefix_mismatch_behavior: "error"`. A replayed thinking block
//! whose bytes, order or binding were wrong would fail the turn.
//!
//! Ignored: it spends Claude OAuth quota with the real credential and stores
//! one owned session. Run explicitly:
//!
//! ```text
//! JCODE_ANTHROPIC_PREFIX_MISMATCH=error cargo test --bin jcode \
//!     claude_child_route_replays -- --ignored --nocapture
//! ```
//!
//! `JCODE_WP02_LIVE_MODEL` selects the model (default `claude-sonnet-5-5`).

use super::register_external_provider_runtimes;
use jcode_base::model_roster::{ModelRoster, ModelRosterRequest, RosterCatalog};
use jcode_provider_core::{ContextReasoningBlockKind, ModelRoute, Provider};
use std::sync::Arc;

struct ClaudeOAuthRoute(String);

#[async_trait::async_trait]
impl Provider for ClaudeOAuthRoute {
    async fn complete(
        &self,
        _: &[jcode_base::message::Message],
        _: &[jcode_base::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<jcode_provider_core::EventStream> {
        anyhow::bail!("catalog only")
    }
    fn name(&self) -> &str {
        "live-catalog"
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        vec![ModelRoute {
            model: self.0.clone(),
            api_method: "claude-oauth".into(),
            provider: "Anthropic".into(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(self.0.clone()))
    }
}

const TASK: &str = "Work step by step and think carefully before each tool call. \
1) Use the read tool to read numbers.txt. \
2) Compute the product of the three numbers modulo 97, then use the write tool to write only that value into first.txt. \
3) Use the read tool to read first.txt. \
4) Compute (that value * 13 + 7) modulo 101, then use the write tool to write only that value into second.txt. \
Make exactly one tool call per step and use only the read and write tools. Then reply with exactly: DONE";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends Claude OAuth quota with the real credential (INT-01/WP-02 live acceptance)"]
async fn claude_child_route_replays_signed_thinking_under_error() -> anyhow::Result<()> {
    use jcode_base::message::{ContentBlock, Role};
    use jcode_provider_anthropic::binding::{ThinkingPayload, thinking_fingerprint};

    anyhow::ensure!(
        std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").as_deref() == Ok("error"),
        "run with JCODE_ANTHROPIC_PREFIX_MISMATCH=error so any mismatch fails the turn"
    );
    let model =
        std::env::var("JCODE_WP02_LIVE_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into());
    register_external_provider_runtimes();
    let catalog = RosterCatalog::from_provider(&ClaudeOAuthRoute(model.clone()))?;
    let roster = ModelRoster::parse(&format!(
        "[aliases.wp02-live]\ndescription='INT-01/WP-02 live acceptance'\nmodels=['claude-oauth:{model}']\ndefault_effort='high'"
    ))?;
    let provider = roster
        .resolve(&ModelRosterRequest::alias("wp02-live"), &catalog)?
        .provider;
    assert_eq!(provider.name(), "anthropic", "the concrete runtime a child uses");
    assert_eq!(
        provider.reasoning_replay_kind(),
        Some(ContextReasoningBlockKind::AnthropicThinking)
    );

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("numbers.txt"), "17\n23\n42\n")?;
    let registry = crate::tool::Registry::new(provider.clone()).await;
    let mut agent = crate::agent::Agent::new(provider, registry);
    agent.set_working_dir(dir.path().to_str().expect("utf-8 temp path"));
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let started = chrono::Utc::now();
    agent
        .run_once_streaming_mpsc(TASK, Vec::new(), None, events)
        .await?;
    let mut stream_errors = Vec::new();
    while let Ok(event) = received.try_recv() {
        if let crate::protocol::ServerEvent::Error { message, .. } = event {
            stream_errors.push(message);
        }
    }
    assert!(stream_errors.is_empty(), "stream errors: {stream_errors:?}");

    let session = crate::session::Session::load(agent.session_id())?;
    let assistant_turns: Vec<_> = session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .collect();
    let mut blocks = Vec::new();
    for (turn, message) in assistant_turns.iter().enumerate() {
        for block in &message.content {
            if let ContentBlock::AnthropicThinking {
                signature, binding, ..
            } = block
            {
                blocks.push((
                    turn,
                    thinking_fingerprint(ThinkingPayload::Signature, signature),
                    binding.clone().expect("fresh blocks are bound"),
                ));
            }
        }
    }
    let turns_with_thinking = blocks
        .iter()
        .map(|(turn, ..)| *turn)
        .collect::<std::collections::BTreeSet<_>>();
    // Every block chains to the one before it in the stored transcript,
    // so each later request replayed the earlier blocks unchanged.
    for pair in blocks.windows(2) {
        assert_eq!(pair[1].2.predecessor.as_ref(), Some(&pair[0].1));
    }
    let binding_events = jcode_provider_core::anthropic_binding_diagnostics::snapshot();
    let tool_calls = assistant_turns
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let written = ["first.txt", "second.txt"].map(|name| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    println!(
        "{}",
        serde_json::json!({
            "route": "claude-oauth (concrete Anthropic runtime, child resolution)",
            "model": model,
            "prefix_mismatch_behavior": "error",
            "started": started.to_rfc3339(),
            "session_id": agent.session_id(),
            "assistant_turns": assistant_turns.len(),
            "tool_calls": tool_calls,
            "thinking_blocks": blocks.len(),
            "turns_with_thinking": turns_with_thinking.len(),
            "block_models": blocks.iter().map(|(_, _, b)| b.model.clone()).collect::<std::collections::BTreeSet<_>>(),
            "binding_events": binding_events.iter().map(|e| format!("{:?}:{}:{}={}", e.source, e.kind, e.reason, e.count)).collect::<Vec<_>>(),
            "written": written,
        })
    );
    assert!(
        turns_with_thinking.len() >= 2,
        "thinking from at least two requests, so later requests replayed earlier blocks"
    );
    assert!(tool_calls >= 3);
    assert!(
        binding_events.is_empty(),
        "no local or provider binding events: {binding_events:?}"
    );
    Ok(())
}
