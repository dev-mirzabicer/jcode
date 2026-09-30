//! INT-01/WP-03 live acceptance: append-only delivery on real routes.
//!
//! A small scripted session runs through the agent's streaming turn loop
//! against a real OAuth route: two turns carrying system reminders, a reload
//! (a fresh Agent from the durable Session) and a reminder-only resume turn,
//! as a reload recovery sends it. On Claude the provider is the concrete
//! Anthropic runtime a child resolves to, which replays signed thinking, and
//! the process must run with `prefix_mismatch_behavior: "error"`: any request
//! whose earlier prefix changed would be rejected. The same script runs on GPT
//! over OpenAI OAuth.
//!
//! Ignored: it spends OAuth quota with the real credential and stores owned
//! sessions. Run explicitly, one route at a time:
//!
//! ```text
//! JCODE_ANTHROPIC_PREFIX_MISMATCH=error JCODE_WP03_LIVE_ROUTE=claude-oauth:claude-sonnet-5-5 \
//!     cargo test --bin jcode append_only_delivery_live -- --ignored --nocapture
//! JCODE_WP03_LIVE_ROUTE=openai-oauth:gpt-5.6-sol \
//!     cargo test --bin jcode append_only_delivery_live -- --ignored --nocapture
//! ```

use super::register_external_provider_runtimes;
use jcode_base::model_roster::{ModelRoster, ModelRosterRequest, RosterCatalog};
use jcode_provider_core::{ModelRoute, Provider};
use std::sync::Arc;

struct LiveRoute {
    api_method: String,
    model: String,
}

#[async_trait::async_trait]
impl Provider for LiveRoute {
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
        let provider = if self.api_method.starts_with("claude") {
            "Anthropic"
        } else {
            "OpenAI"
        };
        vec![ModelRoute {
            model: self.model.clone(),
            api_method: self.api_method.clone(),
            provider: provider.into(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            api_method: self.api_method.clone(),
            model: self.model.clone(),
        })
    }
}

async fn run_turn(
    agent: &mut crate::agent::Agent,
    message: &str,
    reminder: Option<&str>,
) -> anyhow::Result<()> {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc(message, Vec::new(), reminder.map(str::to_string), events)
        .await?;
    while let Ok(event) = received.try_recv() {
        if let crate::protocol::ServerEvent::Error { message, .. } = event {
            anyhow::bail!("stream error: {message}");
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends OAuth quota with the real credential (INT-01/WP-03 live acceptance)"]
async fn append_only_delivery_live() -> anyhow::Result<()> {
    use crate::session::ContextDeliveryChannel;
    use jcode_base::message::{ContentBlock, Role};

    let route = std::env::var("JCODE_WP03_LIVE_ROUTE")
        .unwrap_or_else(|_| "claude-oauth:claude-sonnet-5-5".into());
    let (api_method, model) = route
        .split_once(':')
        .map(|(method, model)| (method.to_string(), model.to_string()))
        .ok_or_else(|| anyhow::anyhow!("JCODE_WP03_LIVE_ROUTE is <api_method>:<model>"))?;
    let claude = api_method.starts_with("claude");
    if claude {
        anyhow::ensure!(
            std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").as_deref() == Ok("error"),
            "run with JCODE_ANTHROPIC_PREFIX_MISMATCH=error so any mismatch fails the turn"
        );
    }
    register_external_provider_runtimes();
    let catalog = RosterCatalog::from_provider(&LiveRoute {
        api_method: api_method.clone(),
        model: model.clone(),
    })?;
    let roster = ModelRoster::parse(&format!(
        "[aliases.wp03-live]\ndescription='INT-01/WP-03 live acceptance'\nmodels=['{route}']\ndefault_effort='low'"
    ))?;
    let resolve = || {
        roster
            .resolve(&ModelRosterRequest::alias("wp03-live"), &catalog)
            .map(|resolved| resolved.provider)
    };
    let provider = resolve()?;
    let replay_kind = provider.reasoning_replay_kind();

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("numbers.txt"), "17\n23\n42\n")?;
    let started = chrono::Utc::now();
    let mut agent =
        crate::agent::Agent::new(provider.clone(), crate::tool::Registry::new(provider).await);
    agent.set_working_dir(dir.path().to_str().expect("utf-8 temp path"));
    run_turn(
        &mut agent,
        "Use the read tool to read numbers.txt, then use the write tool to write the sum of the numbers into sum.txt. Reply with exactly: DONE",
        Some("WP03 harness note: this is an automated acceptance run in a temporary directory."),
    )
    .await?;
    run_turn(
        &mut agent,
        "Use the read tool to read sum.txt, then use the write tool to write that value times two into double.txt. Reply with exactly: DONE",
        Some("WP03 harness note: keep using only the read and write tools."),
    )
    .await?;
    let session_id = agent.session_id().to_string();
    drop(agent);

    // Reload: a fresh Agent resumes from durable state with a reminder-only
    // turn, the shape a reload recovery sends.
    let provider = resolve()?;
    let mut agent = crate::agent::Agent::new_with_session(
        provider.clone(),
        crate::tool::Registry::new(provider).await,
        crate::session::Session::load(&session_id)?,
        None,
    );
    run_turn(
        &mut agent,
        "",
        Some("WP03 resume: use the read tool to read double.txt, then reply with exactly: RESUMED"),
    )
    .await?;

    let session = crate::session::Session::load(&session_id)?;
    let deliveries: Vec<_> = session
        .messages
        .iter()
        .filter_map(|message| message.context_delivery())
        .collect();
    assert_eq!(
        deliveries.len(),
        3,
        "each reminder occurrence delivered once"
    );
    assert!(
        deliveries
            .iter()
            .all(|(channel, _)| *channel == ContextDeliveryChannel::TurnReminder)
    );
    let empty_prompts = session
        .messages
        .iter()
        .filter(|message| {
            message.role == Role::User
                && message.content.iter().all(
                    |block| matches!(block, ContentBlock::Text { text, .. } if text.trim().is_empty()),
                )
        })
        .count();
    assert_eq!(empty_prompts, 0);
    let assistant_turns = session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .count();
    let thinking_blocks = session
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::AnthropicThinking { .. }))
        .count();
    let reasoning_items = session
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::OpenAIReasoning { .. }))
        .count();
    let binding_events = jcode_provider_core::anthropic_binding_diagnostics::snapshot();
    let written = ["sum.txt", "double.txt"].map(|name| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    println!(
        "{}",
        serde_json::json!({
            "route": route,
            "replay_kind": format!("{replay_kind:?}"),
            "prefix_mismatch_behavior": std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").ok(),
            "started": started.to_rfc3339(),
            "session_id": session_id,
            "assistant_turns": assistant_turns,
            "deliveries": deliveries.len(),
            "thinking_blocks": thinking_blocks,
            "openai_reasoning_items": reasoning_items,
            "binding_events": binding_events.iter().map(|e| format!("{:?}:{}:{}={}", e.source, e.kind, e.reason, e.count)).collect::<Vec<_>>(),
            "written": written,
        })
    );
    assert_eq!(written, ["82".to_string(), "164".to_string()]);
    if claude {
        assert!(thinking_blocks >= 2, "thinking was produced and replayed");
        assert!(
            binding_events.is_empty(),
            "no local or provider binding events: {binding_events:?}"
        );
    }
    Ok(())
}
