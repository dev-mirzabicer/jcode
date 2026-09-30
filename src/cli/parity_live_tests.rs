//! INT-01/WP-05 live acceptance (R14): a top-level Claude session over OAuth.
//!
//! The session runs on the production top-level provider (`MultiProvider`,
//! as `jcode --provider claude` builds it) with a shared-session registry, so
//! `subagent` delegation works as it does in the server. The process must run
//! with `prefix_mismatch_behavior: "error"`: any request that replays thinking
//! bound to a changed prefix is rejected, which fails the turn.
//!
//! One leg is about 20 requests: coding turns that think between tool calls,
//! a system reminder, a `subagent` delegation, and a real range summary that
//! is applied, then reverted. Every request's usage is recorded; declared
//! transitions (the summary apply and its revert) are marked. The ledger is
//! printed as JSON before the assertions run.
//!
//! Ignored: it spends Claude OAuth quota (and one curator and one child
//! call) with the real credential, and stores owned sessions. Run one model
//! at a time:
//!
//! ```text
//! JCODE_ANTHROPIC_PREFIX_MISMATCH=error JCODE_WP05_LIVE_MODEL=claude-opus-5-5 \
//!     cargo test -p jcode --lib claude_parity_live -- --ignored --nocapture
//! ```

use jcode_provider_core::CredentialMode;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Requests below this index are the session's first two (R14 measures from
/// request 3).
const CACHE_MEASURED_FROM: usize = 2;
const MIN_CACHE_READ_SHARE: f64 = 0.90;

#[derive(serde::Serialize)]
struct RequestRecord {
    index: usize,
    turn: &'static str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cache_read_share: f64,
    /// The declared transition this request is the first after, if any.
    after_transition: Option<&'static str>,
}

struct Ledger {
    requests: Vec<RequestRecord>,
    pending_transition: Option<&'static str>,
}

impl Ledger {
    fn record(&mut self, turn: &'static str, events: Vec<crate::protocol::ServerEvent>) {
        for event in events {
            let crate::protocol::ServerEvent::TokenUsage {
                input,
                output,
                cache_read_input,
                cache_creation_input,
            } = event
            else {
                continue;
            };
            let cache_read = cache_read_input.unwrap_or(0);
            let cache_write = cache_creation_input.unwrap_or(0);
            let total = input + cache_read + cache_write;
            self.requests.push(RequestRecord {
                index: self.requests.len(),
                turn,
                input,
                output,
                cache_read,
                cache_write,
                cache_read_share: if total == 0 {
                    0.0
                } else {
                    cache_read as f64 / total as f64
                },
                after_transition: self.pending_transition.take(),
            });
        }
    }
}

async fn run_turn(
    agent: &Mutex<crate::agent::Agent>,
    ledger: &mut Ledger,
    turn: &'static str,
    message: &str,
    reminder: Option<&str>,
) -> anyhow::Result<Vec<crate::protocol::ServerEvent>> {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    agent
        .lock()
        .await
        .run_once_streaming_mpsc(message, Vec::new(), reminder.map(str::to_string), events)
        .await?;
    let mut collected = Vec::new();
    while let Ok(event) = received.try_recv() {
        if let crate::protocol::ServerEvent::Error { message, .. } = &event {
            anyhow::bail!("{turn}: stream error: {message}");
        }
        collected.push(event);
    }
    ledger.record(turn, collected.clone());
    Ok(collected)
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

fn bound_thinking<'a>(
    blocks: impl IntoIterator<Item = &'a jcode_base::message::ContentBlock>,
) -> Vec<String> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            jcode_base::message::ContentBlock::AnthropicThinking {
                signature,
                binding: Some(_),
                ..
            } => Some(signature.clone()),
            _ => None,
        })
        .collect()
}

fn tool_calls(events: &[crate::protocol::ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            crate::protocol::ServerEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends Claude OAuth quota with the real credential (INT-01/WP-05 live acceptance)"]
async fn claude_parity_live() -> anyhow::Result<()> {
    use crate::context::{ContextDraftRequest, ContextDraftStatus, ContextMessageRangeSelection};

    anyhow::ensure!(
        std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").as_deref() == Ok("error"),
        "run with JCODE_ANTHROPIC_PREFIX_MISMATCH=error so any mismatch fails the turn"
    );
    let model = std::env::var("JCODE_WP05_LIVE_MODEL").unwrap_or_else(|_| "claude-opus-5-5".into());
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // The production top-level provider, pinned to the OAuth route.
    let provider = crate::cli::provider_init::init_provider_quiet(
        &crate::cli::provider_init::ProviderChoice::Claude,
        Some(&model),
    )
    .await?;
    provider.set_credential_mode(CredentialMode::OAuth)?;
    anyhow::ensure!(provider.model() == model, "model is {}", provider.model());
    anyhow::ensure!(
        provider.reasoning_replay_kind()
            == Some(jcode_provider_core::ContextReasoningBlockKind::AnthropicThinking),
        "top-level Claude replays thinking"
    );
    let effort = provider.reasoning_effort();
    let registry = crate::tool::Registry::new_for_shared_session(
        provider.clone(),
        Arc::new(crate::mcp::SharedMcpPool::new(
            crate::mcp::McpConfig::default(),
        )),
        crate::instruction::InstructionRepositoryService::new(),
    )
    .await?;

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("numbers.txt"), "17\n23\n42\n")?;
    let started = chrono::Utc::now();
    let mut agent = crate::agent::Agent::new(provider.clone(), registry);
    agent.set_working_dir(dir.path().to_str().expect("utf-8 temp path"));
    let session_id = agent.session_id().to_string();
    let agent = Arc::new(Mutex::new(agent));
    let service = Arc::new(crate::context::ContextTransactionService::new());
    let mut ledger = Ledger {
        requests: Vec::new(),
        pending_transition: None,
    };
    let mut transitions = serde_json::Map::new();

    let step = "Work through the arithmetic carefully in your reasoning and double-check it before each tool call.";
    run_turn(
        &agent,
        &mut ledger,
        "first",
        &format!("{step} Use the read tool to read numbers.txt, compute the product of the three numbers modulo 97, then use the write tool to write only that value into first.txt. Reply with exactly: DONE"),
        None,
    )
    .await?;
    let first_turn_end = agent.lock().await.messages().len() - 1;
    run_turn(
        &agent,
        &mut ledger,
        "reminder",
        &format!("{step} Use the read tool to read first.txt, compute (that value cubed + 13) modulo 101 without a calculator, then use the write tool to write only that value into second.txt. Reply with exactly: DONE"),
        Some("# System Reminder\nINT-01 live acceptance: keep final replies to the requested word."),
    )
    .await?;
    let delegation = run_turn(
        &agent,
        &mut ledger,
        "delegation",
        "Call get_catalog to find an isolated-capable agent profile. Then use the subagent tool to create a child with that profile, model alias fast-worker, permission read_only, and exactly this prompt: Reply with exactly: READY. When the child has replied, reply with exactly: DELEGATED",
        None,
    )
    .await?;
    let delegation_calls = tool_calls(&delegation);
    anyhow::ensure!(
        delegation_calls.iter().any(|name| name == "subagent"),
        "the delegation turn called subagent: {delegation_calls:?}"
    );

    // A real range summary of the first turn through the production draft
    // path (curator plan review, draft, apply).
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
        authorization: serde_json::from_value(
            serde_json::json!({"kind": "manual", "initiated_by": "wp05-live"}),
        )?,
    };
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
    transitions.insert("curator_route".into(), serde_json::to_value(&plan.route)?);
    request.curator.expected_plan_fingerprint = Some(plan.fingerprint.clone());
    let draft_id = service.prepare_draft(Arc::clone(&agent), request, false)?;
    let ContextDraftStatus::Ready { draft } = service
        .wait_for_draft(&draft_id, std::time::Duration::from_secs(600))
        .await?
    else {
        anyhow::bail!("the summary draft did not become ready");
    };
    transitions.insert(
        "review".into(),
        serde_json::to_value(&draft.preview.reasoning_invalidation)?,
    );
    let applied = service.apply_draft(&agent, &draft_id, None, false)?;
    let transaction_id = applied.transaction.id.clone();
    transitions.insert(
        "apply".into(),
        serde_json::to_value(&applied.reasoning_invalidation)?,
    );
    ledger.pending_transition = Some("summary apply");

    run_turn(
        &agent,
        &mut ledger,
        "after-summary",
        &format!("{step} Use the read tool to read second.txt, compute (7 times that value squared, plus 11) modulo 50 without a calculator, then use the write tool to write only that value into third.txt. Reply with exactly: DONE"),
        None,
    )
    .await?;

    let reverted = service.revert_transaction(&agent, &transaction_id, false)?;
    transitions.insert(
        "revert".into(),
        serde_json::to_value(&reverted.reasoning_invalidation)?,
    );
    ledger.pending_transition = Some("summary revert");

    run_turn(
        &agent,
        &mut ledger,
        "after-revert",
        &format!("{step} Use the read tool to read third.txt, compute (that value to the fourth power, plus that value, plus 1) modulo 41 without a calculator, then use the write tool to write only that value into fourth.txt. Reply with exactly: DONE"),
        None,
    )
    .await?;
    run_turn(
        &agent,
        &mut ledger,
        "continue",
        &format!("{step} Use the read tool to read fourth.txt, compute (that value squared plus 3) modulo 13 without a calculator, then use the write tool to write only that value into fifth.txt. Reply with exactly: DONE"),
        None,
    )
    .await?;
    run_turn(
        &agent,
        &mut ledger,
        "final",
        &format!("{step} Use the read tool to read fifth.txt, compute that value cubed modulo 11 without a calculator, then reply with exactly: FINISHED"),
        None,
    )
    .await?;

    let mut session = crate::session::Session::load(&session_id)?;
    let projected = session.projected_messages_for_provider()?;
    let stored = bound_thinking(session.messages.iter().flat_map(|m| &m.content));
    let replayed = bound_thinking(projected.iter().flat_map(|m| &m.content));
    let suppressed = session
        .context_view
        .active_reasoning_invalidation()
        .map(|transaction| transaction.operations.len());
    let written = [
        "first.txt",
        "second.txt",
        "third.txt",
        "fourth.txt",
        "fifth.txt",
    ]
    .map(|name| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let events = binding_events();
    let measured: Vec<&RequestRecord> = ledger
        .requests
        .iter()
        .filter(|record| record.index >= CACHE_MEASURED_FROM && record.after_transition.is_none())
        .collect();
    let below: Vec<usize> = measured
        .iter()
        .filter(|record| record.cache_read_share < MIN_CACHE_READ_SHARE)
        .map(|record| record.index)
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "route": "claude-oauth (MultiProvider top-level)",
            "model": model,
            "effort": effort,
            "prefix_mismatch_behavior": "error",
            "started": started.to_rfc3339(),
            "finished": chrono::Utc::now().to_rfc3339(),
            "session_id": session_id,
            "requests": ledger.requests,
            "measured_requests": measured.len(),
            "below_cache_threshold": below,
            "transitions": transitions,
            "delegation_tool_calls": delegation_calls,
            "stored_bound_thinking": stored.len(),
            "replayed_in_final_projection": replayed.len(),
            "managed_suppressed_operations": suppressed,
            "binding_events": events,
            "written": written,
        }))?
    );

    assert!(
        ledger.requests.len() >= 20,
        "a leg is at least 20 requests, got {}",
        ledger.requests.len()
    );
    assert!(
        events.is_empty(),
        "no local or provider binding events: {events:?}"
    );
    assert!(
        !replayed.is_empty(),
        "thinking was produced and is replayed"
    );
    assert!(
        replayed.iter().all(|signature| stored.contains(signature)),
        "every replayed signature is an original stored signature"
    );
    assert!(
        below.is_empty(),
        "cache reads below {MIN_CACHE_READ_SHARE} of input at requests {below:?}"
    );
    Ok(())
}

/// INT-01/WP-05 (R13): effort `none` in its production shape on the models
/// where it changed: `thinking: {type: "disabled"}` on Opus 5, and `low`
/// effort on Opus 5.5, whose thinking cannot be turned off. One short request
/// per model over Claude OAuth.
///
/// ```text
/// cargo test -p jcode --lib effort_none_shapes_live -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends Claude OAuth quota with the real credential (INT-01/WP-05 live acceptance)"]
async fn effort_none_shapes_live() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut outcomes = Vec::new();
    for (model, surfaced) in [("claude-opus-5", "none"), ("claude-opus-5-5", "low")] {
        let provider = crate::cli::provider_init::init_provider_quiet(
            &crate::cli::provider_init::ProviderChoice::Claude,
            Some(model),
        )
        .await?;
        provider.set_credential_mode(CredentialMode::OAuth)?;
        provider.set_reasoning_effort("none")?;
        anyhow::ensure!(
            provider.reasoning_effort().as_deref() == Some(surfaced),
            "{model}: effort surfaces as {:?}",
            provider.reasoning_effort()
        );
        let registry = crate::tool::Registry::new(provider.clone()).await;
        let mut agent = crate::agent::Agent::new(provider, registry);
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        agent
            .run_once_streaming_mpsc("Reply with exactly: OK", Vec::new(), None, events)
            .await?;
        let mut error = None;
        let mut usage = None;
        while let Ok(event) = received.try_recv() {
            match event {
                crate::protocol::ServerEvent::Error { message, .. } => error = Some(message),
                crate::protocol::ServerEvent::TokenUsage { input, output, .. } => {
                    usage = Some((input, output))
                }
                _ => {}
            }
        }
        outcomes.push(serde_json::json!({
            "model": model,
            "effort": surfaced,
            "error": error,
            "usage": usage,
            "session_id": agent.session_id(),
            "at": chrono::Utc::now().to_rfc3339(),
        }));
    }
    println!("{}", serde_json::to_string_pretty(&outcomes)?);
    for outcome in &outcomes {
        assert!(outcome["error"].is_null(), "{outcome}");
        assert!(!outcome["usage"].is_null(), "{outcome}");
    }
    Ok(())
}
