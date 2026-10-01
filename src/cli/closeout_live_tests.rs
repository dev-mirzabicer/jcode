//! INT-01 closeout journeys, live, on the production top-level provider.
//!
//! One script runs on Claude over OAuth (ACCEPTANCE journey 1) and on GPT over
//! OpenAI OAuth (journey 2): a multi-tool turn that uses `batch`, a `subagent`
//! delegation, a skill activation followed by a turn carrying a system
//! reminder, a real range summary that is applied, continued work, then a
//! reload into a fresh Agent from the durable Session that resumes with a
//! reminder-only turn, as a reload recovery sends it (journey 3), and a final
//! turn. Every request's usage is recorded and the declared transitions (the
//! skill activation and the summary apply) are marked. The reload is not a
//! transition: the request after it must read the cache like any other.
//!
//! On Claude the process must run with `prefix_mismatch_behavior: "error"`,
//! so any request whose earlier prefix changed is rejected.
//!
//! Ignored: it spends OAuth quota (plus one curator and one child call) with
//! the real credential, and stores owned sessions. Run one route at a time:
//!
//! ```text
//! JCODE_ANTHROPIC_PREFIX_MISMATCH=error JCODE_CLOSEOUT_LIVE_ROUTE=claude:claude-opus-5-5 \
//!     cargo test -p jcode --lib closeout_journeys_live -- --ignored --nocapture
//! JCODE_CLOSEOUT_LIVE_ROUTE=openai:gpt-5.6-sol \
//!     cargo test -p jcode --lib closeout_journeys_live -- --ignored --nocapture
//! ```
//!
//! `JCODE_CLOSEOUT_SKILL` names the skill to activate (default: the first
//! available skill).

use crate::cli::provider_init::{ProviderChoice, init_provider_quiet};
use jcode_base::message::{ContentBlock, Role};
use jcode_provider_core::{
    ContextReasoningBlockKind, CredentialMode, Provider, ReasoningBinding,
    anthropic_reasoning_binding,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Requests below this index are the session's first two.
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
    after_transition: Option<&'static str>,
}

struct Ledger {
    family: &'static str,
    requests: Vec<RequestRecord>,
    pending_transition: Option<&'static str>,
}

impl Ledger {
    fn record(&mut self, turn: &'static str, events: &[crate::protocol::ServerEvent]) {
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
            let total = jcode_provider_core::effective_context_tokens_from_usage(
                self.family,
                *input,
                *cache_read_input,
                *cache_creation_input,
            );
            self.requests.push(RequestRecord {
                index: self.requests.len(),
                turn,
                input: *input,
                output: *output,
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
) -> anyhow::Result<Vec<String>> {
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
    ledger.record(turn, &collected);
    Ok(collected
        .iter()
        .filter_map(|event| match event {
            crate::protocol::ServerEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect())
}

async fn shared_registry(provider: Arc<dyn Provider>) -> anyhow::Result<crate::tool::Registry> {
    crate::tool::Registry::new_for_shared_session(
        provider,
        Arc::new(crate::mcp::SharedMcpPool::new(
            crate::mcp::McpConfig::default(),
        )),
        crate::instruction::InstructionRepositoryService::new(),
    )
    .await
}

fn bound_thinking<'a>(blocks: impl IntoIterator<Item = &'a ContentBlock>) -> Vec<String> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::AnthropicThinking {
                signature,
                binding: Some(_),
                ..
            } => Some(signature.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends OAuth quota with the real credential (INT-01 closeout journeys)"]
async fn closeout_journeys_live() -> anyhow::Result<()> {
    use crate::context::{ContextDraftRequest, ContextDraftStatus, ContextMessageRangeSelection};

    let route = std::env::var("JCODE_CLOSEOUT_LIVE_ROUTE")
        .unwrap_or_else(|_| "claude:claude-opus-5-5".into());
    let (family, model) = route
        .split_once(':')
        .map(|(family, model)| (family.to_string(), model.to_string()))
        .ok_or_else(|| anyhow::anyhow!("JCODE_CLOSEOUT_LIVE_ROUTE is <claude|openai>:<model>"))?;
    let claude = match family.as_str() {
        "claude" => true,
        "openai" => false,
        other => anyhow::bail!("unknown route family {other}"),
    };
    if claude {
        anyhow::ensure!(
            std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").as_deref() == Ok("error"),
            "run with JCODE_ANTHROPIC_PREFIX_MISMATCH=error so any mismatch fails the turn"
        );
    }
    // The binary installs this in `main`; the OpenAI transport needs it.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let choice = if claude {
        ProviderChoice::Claude
    } else {
        ProviderChoice::Openai
    };
    let provider = init_provider_quiet(&choice, Some(&model)).await?;
    if claude {
        provider.set_credential_mode(CredentialMode::OAuth)?;
    }
    anyhow::ensure!(provider.model() == model, "model is {}", provider.model());
    let expected_replay = if claude {
        ContextReasoningBlockKind::AnthropicThinking
    } else {
        ContextReasoningBlockKind::OpenAiReasoning
    };
    anyhow::ensure!(
        provider.reasoning_replay_kind() == Some(expected_replay),
        "the top-level provider replays reasoning: {:?}",
        provider.reasoning_replay_kind()
    );
    let binds = claude && anthropic_reasoning_binding(&model) == ReasoningBinding::PrefixBound;
    let effort = provider.reasoning_effort();

    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("a.txt"), "17\n23\n")?;
    std::fs::write(dir.path().join("b.txt"), "42\n")?;
    let working_dir = dir.path().to_str().expect("utf-8 temp path").to_string();
    let started = chrono::Utc::now();
    let mut agent =
        crate::agent::Agent::new(provider.clone(), shared_registry(provider.clone()).await?);
    agent.set_working_dir(&working_dir);
    let session_id = agent.session_id().to_string();
    let skill = match std::env::var("JCODE_CLOSEOUT_SKILL") {
        Ok(name) => name,
        Err(_) => agent
            .available_skill_names()
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no skill is available to activate"))?,
    };
    let agent = Arc::new(Mutex::new(agent));
    let service = Arc::new(crate::context::ContextTransactionService::new());
    let mut ledger = Ledger {
        family: if claude { "claude" } else { "openai" },
        requests: Vec::new(),
        pending_transition: None,
    };
    let mut transitions = serde_json::Map::new();
    let mut calls = serde_json::Map::new();

    let step = "Work through the arithmetic carefully in your reasoning and double-check it before each tool call.";
    let batch_calls = run_turn(
        &agent,
        &mut ledger,
        "batch",
        &format!("{step} In one batch tool call, read both a.txt and b.txt with the read tool. Compute the product of the three numbers they contain modulo 97, then use the write tool to write only that value into first.txt. Reply with exactly: DONE"),
        None,
    )
    .await?;
    calls.insert("batch".into(), serde_json::to_value(&batch_calls)?);
    anyhow::ensure!(
        batch_calls.iter().any(|name| name == "batch"),
        "the first turn used batch: {batch_calls:?}"
    );
    let first_turn_end = agent.lock().await.messages().len() - 1;

    let delegation_calls = run_turn(
        &agent,
        &mut ledger,
        "delegation",
        "Call get_catalog to find an isolated-capable agent profile. Then use the subagent tool to create a child with that profile, model alias fast-worker, permission read_only, and exactly this prompt: Reply with exactly: READY. When the child has replied, reply with exactly: DELEGATED",
        None,
    )
    .await?;
    calls.insert(
        "delegation".into(),
        serde_json::to_value(&delegation_calls)?,
    );
    anyhow::ensure!(
        delegation_calls.iter().any(|name| name == "subagent"),
        "the delegation turn called subagent: {delegation_calls:?}"
    );

    // A skill activation is a declared static-prompt transition.
    let activation = agent.lock().await.activate_skill(&skill)?;
    transitions.insert("skill".into(), serde_json::json!(activation.skill_id));
    ledger.pending_transition = Some("skill activation");
    run_turn(
        &agent,
        &mut ledger,
        "reminder",
        &format!("{step} Use the read tool to read first.txt, compute (that value cubed + 13) modulo 101 without a calculator, then use the write tool to write only that value into second.txt. Reply with exactly: DONE"),
        Some("# System Reminder\nINT-01 closeout journey: keep final replies to the requested word."),
    )
    .await?;

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
            serde_json::json!({"kind": "manual", "initiated_by": "int01-closeout-live"}),
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
    request.curator.expected_plan_fingerprint = Some(plan.fingerprint.clone());
    let draft_id = service.prepare_draft(Arc::clone(&agent), request, false)?;
    let ContextDraftStatus::Ready { draft } = service
        .wait_for_draft(&draft_id, std::time::Duration::from_secs(600))
        .await?
    else {
        anyhow::bail!("the summary draft did not become ready");
    };
    transitions.insert(
        "summary_review".into(),
        serde_json::to_value(&draft.preview.reasoning_invalidation)?,
    );
    let applied = service.apply_draft(&agent, &draft_id, None, false)?;
    transitions.insert(
        "summary_apply".into(),
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
    let messages_before_reload = agent.lock().await.messages().len();
    drop(agent);

    // Reload: a fresh Agent and registry resume from durable state with a
    // reminder-only turn, the shape a reload recovery sends.
    let provider = init_provider_quiet(&choice, Some(&model)).await?;
    if claude {
        provider.set_credential_mode(CredentialMode::OAuth)?;
    }
    let mut resumed = crate::agent::Agent::new_with_session(
        provider.clone(),
        shared_registry(provider.clone()).await?,
        crate::session::Session::load(&session_id)?,
        None,
    );
    resumed.set_working_dir(&working_dir);
    let agent = Arc::new(Mutex::new(resumed));
    anyhow::ensure!(
        agent.lock().await.messages().len() == messages_before_reload,
        "the reload restored every message"
    );
    run_turn(
        &agent,
        &mut ledger,
        "resume",
        "",
        Some("INT-01 closeout resume: think carefully, use the read tool to read third.txt, compute (that value squared plus 3) modulo 13 without a calculator, then use the write tool to write only that value into fourth.txt. Reply with exactly: RESUMED"),
    )
    .await?;
    run_turn(
        &agent,
        &mut ledger,
        "final",
        &format!("{step} Use the read tool to read fourth.txt, compute that value cubed modulo 11 without a calculator, then reply with exactly: FINISHED"),
        None,
    )
    .await?;

    let mut session = crate::session::Session::load(&session_id)?;
    let projected = session.projected_messages_for_provider()?;
    let stored = bound_thinking(session.messages.iter().flat_map(|m| &m.content));
    let replayed = bound_thinking(projected.iter().flat_map(|m| &m.content));
    let openai_items = session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| matches!(block, ContentBlock::OpenAIReasoning { .. }))
        .count();
    let deliveries: Vec<String> = session
        .messages
        .iter()
        .filter_map(|message| message.context_delivery())
        .map(|(channel, _)| format!("{channel:?}"))
        .collect();
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
    let synthetic_continuations = session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| matches!(block, ContentBlock::Text { text, .. } if text == "Continue."))
        .count();
    let tool_errors = session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| {
            matches!(
                block,
                ContentBlock::ToolResult {
                    is_error: Some(true),
                    ..
                }
            )
        })
        .count();
    let suppressed = session
        .context_view
        .active_reasoning_invalidation()
        .map(|transaction| transaction.operations.len());
    let binding_events: Vec<String> =
        jcode_provider_core::anthropic_binding_diagnostics::snapshot()
            .iter()
            .map(|event| {
                format!(
                    "{:?}:{}:{}={}",
                    event.source, event.kind, event.reason, event.count
                )
            })
            .collect();
    let written = ["first.txt", "second.txt", "third.txt", "fourth.txt"].map(|name| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    });
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
            "route": route,
            "effort": effort,
            "binds_thinking": binds,
            "prefix_mismatch_behavior": std::env::var("JCODE_ANTHROPIC_PREFIX_MISMATCH").ok(),
            "started": started.to_rfc3339(),
            "finished": chrono::Utc::now().to_rfc3339(),
            "session_id": session_id,
            "requests": ledger.requests,
            "measured_requests": measured.len(),
            "below_cache_threshold": below,
            "transitions": transitions,
            "tool_calls": calls,
            "deliveries": deliveries,
            "empty_prompts": empty_prompts,
            "synthetic_continuations": synthetic_continuations,
            "tool_errors": tool_errors,
            "stored_bound_thinking": stored.len(),
            "replayed_in_final_projection": replayed.len(),
            "openai_reasoning_items": openai_items,
            "managed_suppressed_operations": suppressed,
            "binding_events": binding_events,
            "written": written,
        }))?
    );

    assert_eq!(
        deliveries,
        ["TurnReminder", "TurnReminder"],
        "each reminder delivered once"
    );
    assert_eq!(empty_prompts, 0, "no empty prompt stored");
    assert_eq!(
        synthetic_continuations, 0,
        "no synthetic continuation stored"
    );
    assert_eq!(tool_errors, 0, "no tool call failed");
    assert!(
        written.iter().all(|value| !value.is_empty()),
        "every step wrote its file"
    );
    if claude {
        assert!(
            binding_events.is_empty(),
            "no local or provider binding events: {binding_events:?}"
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
    } else {
        assert!(
            openai_items > 0,
            "GPT reasoning items were stored for replay"
        );
    }
    Ok(())
}
