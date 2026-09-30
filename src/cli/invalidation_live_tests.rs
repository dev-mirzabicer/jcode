//! INT-01/WP-04 live acceptance: context-control transitions on a real
//! prefix-bound Claude route.
//!
//! A scripted session runs on the concrete Anthropic runtime a child
//! resolves to, which replays signed thinking, with
//! `prefix_mismatch_behavior: "error"`: any request that still replays a block
//! bound to a changed prefix is rejected. The session then applies a real
//! range summary through the production draft path (curator generation,
//! review, apply), continues, reverts it, continues, reapplies it and
//! continues again. Every continuation must be accepted, and each transition
//! must stage or restore the thinking it affects.
//!
//! Ignored: it spends OAuth quota (coding turns and one curator call) with the
//! real credential and stores an owned session. Run explicitly:
//!
//! ```text
//! JCODE_ANTHROPIC_PREFIX_MISMATCH=error \
//!     cargo test -p jcode --lib context_transitions_live -- --ignored --nocapture
//! ```
//!
//! `JCODE_WP04_LIVE_ROUTE` (default `claude-oauth:claude-sonnet-5-5`) and
//! `JCODE_WP04_LIVE_EFFORT` (default `high`) select the coding route.

use super::register_external_provider_runtimes;
use jcode_base::model_roster::{ModelRoster, ModelRosterRequest, RosterCatalog};
use jcode_provider_core::{ModelRoute, Provider};
use std::sync::Arc;
use tokio::sync::Mutex;

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
        vec![ModelRoute {
            model: self.model.clone(),
            api_method: self.api_method.clone(),
            provider: "Anthropic".into(),
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

async fn run_turn(agent: &Mutex<crate::agent::Agent>, message: &str) -> anyhow::Result<()> {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    agent
        .lock()
        .await
        .run_once_streaming_mpsc(message, Vec::new(), None, events)
        .await?;
    while let Ok(event) = received.try_recv() {
        if let crate::protocol::ServerEvent::Error { message, .. } = event {
            anyhow::bail!("stream error: {message}");
        }
    }
    Ok(())
}

fn binding_events() -> Vec<String> {
    jcode_provider_core::anthropic_binding_diagnostics::snapshot()
        .iter()
        .map(|event| {
            format!(
                "{:?}:{}:{}={}",
                event.source, event.kind, event.reason, event.count
            )
        })
        .collect()
}

fn thinking_count(session: &crate::session::Session) -> usize {
    session
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| {
            matches!(
                block,
                jcode_base::message::ContentBlock::AnthropicThinking {
                    binding: Some(_),
                    ..
                }
            )
        })
        .count()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends OAuth quota with the real credential (INT-01/WP-04 live acceptance)"]
async fn context_transitions_live() -> anyhow::Result<()> {
    use crate::context::{ContextDraftRequest, ContextDraftStatus, ContextMessageRangeSelection};
    use jcode_base::message::Role;

    anyhow::ensure!(
        std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").as_deref() == Ok("error"),
        "run with JCODE_ANTHROPIC_PREFIX_MISMATCH=error so any mismatch fails the turn"
    );
    let route = std::env::var("JCODE_WP04_LIVE_ROUTE")
        .unwrap_or_else(|_| "claude-oauth:claude-sonnet-5-5".into());
    let (api_method, model) = route
        .split_once(':')
        .map(|(method, model)| (method.to_string(), model.to_string()))
        .ok_or_else(|| anyhow::anyhow!("JCODE_WP04_LIVE_ROUTE is <api_method>:<model>"))?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    register_external_provider_runtimes();
    let catalog = RosterCatalog::from_provider(&LiveRoute { api_method, model })?;
    let effort = std::env::var("JCODE_WP04_LIVE_EFFORT").unwrap_or_else(|_| "high".into());
    let roster = ModelRoster::parse(&format!(
        "[aliases.wp04-live]\ndescription='INT-01/WP-04 live acceptance'\nmodels=['{route}']\ndefault_effort='{effort}'"
    ))?;
    let provider = roster
        .resolve(&ModelRosterRequest::alias("wp04-live"), &catalog)?
        .provider;

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("numbers.txt"), "17\n23\n42\n")?;
    let started = chrono::Utc::now();
    let mut agent =
        crate::agent::Agent::new(provider.clone(), crate::tool::Registry::new(provider).await);
    agent.set_working_dir(dir.path().to_str().expect("utf-8 temp path"));
    let session_id = agent.session_id().to_string();
    let agent = Arc::new(Mutex::new(agent));
    let service = Arc::new(crate::context::ContextTransactionService::new());
    let mut record = serde_json::Map::new();

    let step = "Work through the arithmetic carefully in your reasoning and double-check it before each tool call.";
    run_turn(
        &agent,
        &format!("{step} Use the read tool to read numbers.txt, compute the product of the three numbers modulo 97, then use the write tool to write only that value into first.txt. Reply with exactly: DONE"),
    )
    .await?;
    let first_turn_end = agent.lock().await.messages().len() - 1;
    run_turn(
        &agent,
        &format!("{step} Use the read tool to read first.txt, compute (that value cubed + 13) modulo 101 without a calculator, then use the write tool to write only that value into second.txt. Reply with exactly: DONE"),
    )
    .await?;

    // A real range summary of the first turn, prepared and applied through
    // the production draft path.
    let (start, end) = {
        let guard = agent.lock().await;
        let messages = guard.messages();
        (messages[0].id.clone(), messages[first_turn_end].id.clone())
    };
    let mut request = ContextDraftRequest {
        summary_ranges: vec![ContextMessageRangeSelection {
            start_message_id: start,
            end_message_id: end,
        }],
        reasoning: None,
        tool_results: Vec::new(),
        allow_shadowing_active_operations: false,
        curator: Default::default(),
        // Manual authorization, in its wire form (the root crate does not
        // name the session-types crate).
        authorization: serde_json::from_value(
            serde_json::json!({"kind": "manual", "initiated_by": "wp04-live"}),
        )?,
    };
    // The editor's Exact Calls step: review the curator plan, then prepare
    // with its fingerprint.
    let plan = {
        let mut guard = agent.lock().await;
        let snapshot = service.context_editor_snapshot(&mut guard, false)?;
        service.preview_context_curator_plan(
            &mut guard,
            false,
            snapshot.context_revision,
            snapshot.transcript_digest,
            request.clone(),
        )?
    };
    record.insert("curator_route".into(), serde_json::to_value(&plan.route)?);
    request.curator.expected_plan_fingerprint = Some(plan.fingerprint.clone());
    let draft_id = service.prepare_draft(Arc::clone(&agent), request, false)?;
    let ContextDraftStatus::Ready { draft } = service
        .wait_for_draft(&draft_id, std::time::Duration::from_secs(600))
        .await?
    else {
        anyhow::bail!("the summary draft did not become ready");
    };
    let reviewed = draft.preview.reasoning_invalidation.clone();
    anyhow::ensure!(
        reviewed
            .as_ref()
            .is_some_and(|summary| summary.invalidated_by_change > 0),
        "the review lists the second turn's thinking as invalidated: {reviewed:?}"
    );
    record.insert("review".into(), serde_json::to_value(&reviewed)?);
    let applied = service.apply_draft(&agent, &draft_id, None, false)?;
    let transaction_id = applied.transaction.id.clone();
    record.insert(
        "apply".into(),
        serde_json::to_value(&applied.reasoning_invalidation)?,
    );
    anyhow::ensure!(
        applied
            .reasoning_invalidation
            .as_ref()
            .map(|s| s.invalidated_by_change)
            == reviewed.as_ref().map(|s| s.invalidated_by_change),
        "apply staged what the review showed"
    );

    run_turn(
        &agent,
        &format!("{step} Use the read tool to read second.txt, compute (7 times that value squared, plus 11) modulo 50 without a calculator, then use the write tool to write only that value into third.txt. Reply with exactly: DONE"),
    )
    .await?;

    let reverted = service.revert_transaction(&agent, &transaction_id, false)?;
    record.insert(
        "revert".into(),
        serde_json::to_value(&reverted.reasoning_invalidation)?,
    );
    run_turn(
        &agent,
        &format!("{step} Use the read tool to read third.txt, compute (that value to the fourth power, plus that value, plus 1) modulo 41 without a calculator, then use the write tool to write only that value into fourth.txt. Reply with exactly: DONE"),
    )
    .await?;

    let reapplied = service.reapply_transaction(&agent, &transaction_id, false)?;
    record.insert(
        "reapply".into(),
        serde_json::to_value(&reapplied.reasoning_invalidation)?,
    );
    run_turn(
        &agent,
        &format!("{step} Use the read tool to read fourth.txt, compute that value cubed modulo 11 without a calculator, then reply with exactly: FINISHED"),
    )
    .await?;

    let session = crate::session::Session::load(&session_id)?;
    let written = ["first.txt", "second.txt", "third.txt", "fourth.txt"].map(|name| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let managed = session
        .context_view
        .transactions
        .iter()
        .filter(|transaction| transaction.is_reasoning_invalidation())
        .count();
    let events = binding_events();
    println!(
        "{}",
        serde_json::json!({
            "route": route,
            "effort": effort,
            "prefix_mismatch_behavior": "error",
            "started": started.to_rfc3339(),
            "finished": chrono::Utc::now().to_rfc3339(),
            "session_id": session_id,
            "transaction_id": transaction_id,
            "context_revision": session.context_view.revision,
            "assistant_turns": session.messages.iter().filter(|m| m.role == Role::Assistant).count(),
            "bound_thinking_blocks": thinking_count(&session),
            "managed_invalidation_transactions": managed,
            "active_invalidated_blocks": session.context_view.active_reasoning_invalidation().map(|t| t.operations.len()),
            "transitions": record,
            "binding_events": events,
            "written": written,
        })
    );
    assert_eq!(written, ["29", "61", "8", "5"].map(String::from));
    assert!(thinking_count(&session) >= 4, "thinking was produced");
    assert!(managed >= 2, "the transitions changed the managed set");
    assert!(
        events.is_empty(),
        "no local or provider binding events: {events:?}"
    );
    Ok(())
}
