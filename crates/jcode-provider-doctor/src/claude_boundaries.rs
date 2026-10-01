//! Live Claude OAuth boundary probes (INT-01/WP-06).
//!
//! `jcode provider-doctor claude --contract claude-oauth-boundaries --model <id>`
//! asks the questions WP-06's design depends on, through jcode's own request
//! builder, token resolution and attribution headers:
//!
//! - G6.1: mid-conversation `role: "system"` messages after a user prompt and
//!   after tool results, two adjacent ones, `cache_control` on one, and
//!   thinking replayed across one under `prefix_mismatch_behavior: "error"`;
//! - G6.2: tool changes by value (`inline-tools-2026-09-15`) and by reference
//!   (`mid-conversation-tool-changes-2026-07-01`), each under `error`, with the
//!   cache read and the next turn's thinking checked;
//! - G6.4: a replayed thinking block whose summarized text was modified;
//! - G6.5: Opus 5.5, two tool turns on Sonnet 5.5, back to Opus 5.5, under
//!   `error`, recording every `input_transformations` entry (Opus 5.5 only);
//! - G6.6: the OAuth billing-header version changed under `error`.
//!
//! Nothing here has a recorded expectation: each probe decides a question and
//! the report records the outcome. Every request spends subscription quota.

use crate::claude_contract::{ContractProbe, ContractReport, Gate0Expectation};
use anyhow::{Context, Result};
use jcode_base::message::{Message, ToolDefinition};
use jcode_base::provider::Provider;
use jcode_provider_anthropic_runtime::AnthropicProvider;
use serde_json::{Value, json};

pub const CLAUDE_OAUTH_BOUNDARY_CONTRACT: &str = "claude-oauth-boundaries";

const PROBE_SYSTEM: &str = "You are a probe assistant for an API compatibility test. Follow the user's instructions exactly and keep answers minimal.";

const OPERATOR_NOTE: &str =
    "Operator note: when you next reply in text, end your reply with the word NOTED.";

const THINKING_TASK: &str = "Think it through step by step before acting: a train leaves at 14:37 and travels 283 km at 91 km/h, then waits 17 minutes, then travels 146 km at 73 km/h. Compute the exact arrival time (HH:MM, rounded down), then use the bash tool to run exactly `echo parity-probe <HH:MM>` with your answer. After you see the result, reply with exactly: DONE";

/// Follow-up steps for the model-switch probe, each needing fresh reasoning
/// and a tool call.
const SWITCH_STEPS: [&str; 4] = [
    "Now think it through again: after arriving, the train continues 97 km at 83 km/h. Compute the new exact arrival time (HH:MM, rounded down), then use the bash tool to run exactly `echo step-1 <HH:MM>` with your answer.",
    "Think carefully: 7 workers build 3 walls in 11 hours. How many minutes (rounded down) do 5 workers need for 4 walls at the same rate? Use the bash tool to run exactly `echo step-2 <minutes>` with your answer.",
    "Think carefully: what is the sum of the prime numbers between 100 and 140? Use the bash tool to run exactly `echo step-3 <sum>` with your answer.",
    "Think carefully: a tank holds 1250 litres; it drains 37 litres per minute while 12 litres per minute flow in. After how many whole minutes is it first below 400 litres? Use the bash tool to run exactly `echo step-4 <minutes>` with your answer.",
];

/// Models whose documentation lists mid-conversation `role: "system"`
/// messages and tool changes (Sonnet 5 is not listed).
const SWITCH_SECOND_MODEL: &str = "claude-sonnet-5-5";

struct Probes<'a> {
    provider: &'a AnthropicProvider,
    probes: Vec<ContractProbe>,
    notes: Vec<String>,
}

impl Probes<'_> {
    async fn send(
        &mut self,
        id: &'static str,
        question: &'static str,
        body: &Value,
    ) -> Result<Value> {
        let (status, response) = self
            .provider
            .send_contract_request_for_doctor(body)
            .await
            .with_context(|| format!("probe {id}"))?;
        self.probes.push(ContractProbe {
            id,
            question,
            gate0: Gate0Expectation::Undetermined,
            status,
            observed: classify(status, &response),
            agrees_with_gate0: None,
            summary: summarize(&response),
            response: crate::claude_contract::redact(&response),
        });
        Ok(response)
    }
}

fn classify(status: u16, response: &Value) -> Gate0Expectation {
    crate::claude_contract::classify(status, response)
}

/// A summary line plus the first text block, so instruction-following and
/// reasoning presence are visible without opening the report.
fn summarize(response: &Value) -> String {
    let text = response
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|block| block["type"] == "text")
        .and_then(|block| block["text"].as_str())
        .map(|text| text.chars().take(120).collect::<String>())
        .unwrap_or_default();
    format!(
        "{} text={:?}",
        crate::claude_contract::summarize(response),
        text
    )
}

fn content_of(response: &Value) -> Value {
    response
        .get("content")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn tool_use_id(content: &Value) -> Option<String> {
    content
        .as_array()?
        .iter()
        .find(|block| block["type"] == "tool_use")?["id"]
        .as_str()
        .map(str::to_string)
}

fn has_thinking(content: &Value) -> bool {
    content
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "thinking"))
}

fn with_binding(mut body: Value, behavior: &str) -> Value {
    if body.get("thinking").is_none() {
        body["thinking"] = json!({"type": "adaptive"});
    }
    body["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior": behavior});
    body
}

fn user_text(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

fn system_text(text: &str) -> Value {
    json!({"role": "system", "content": [{"type": "text", "text": text}]})
}

/// The user turn answering an assistant turn: its tool result when it called
/// a tool, otherwise a short prompt.
fn answer(content: &Value, result: &str) -> Value {
    match tool_use_id(content) {
        Some(id) => json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": result}
        ]}),
        None => user_text("Reply with exactly: OK"),
    }
}

/// The first turn every replay probe builds on: the thinking task, its
/// response with signed thinking before a tool call, and the tool result.
struct ThinkingTurn {
    request: Value,
    messages: Vec<Value>,
}

async fn thinking_turn(
    probes: &mut Probes<'_>,
    id: &'static str,
    bash: &[ToolDefinition],
) -> Result<Option<ThinkingTurn>> {
    probes.provider.set_reasoning_effort("high")?;
    let request = with_binding(
        probes
            .provider
            .contract_request_body_for_doctor(&[Message::user(THINKING_TASK)], bash, PROBE_SYSTEM)
            .await?,
        "error",
    );
    let response = probes
        .send(
            id,
            "the thinking turn the replay probes build on (signed thinking before a tool call)",
            &request,
        )
        .await?;
    let content = content_of(&response);
    if !has_thinking(&content) || tool_use_id(&content).is_none() {
        probes.notes.push(format!(
            "{id} produced no signed thinking before a tool_use; the probes that replay it did not run"
        ));
        return Ok(None);
    }
    let messages = vec![
        user_text(THINKING_TASK),
        json!({"role": "assistant", "content": content}),
        answer(&content, "parity-probe 21:24"),
    ];
    Ok(Some(ThinkingTurn { request, messages }))
}

fn body_with(base: &Value, messages: Vec<Value>) -> Value {
    let mut body = base.clone();
    body["messages"] = Value::Array(messages);
    body
}

/// Continue a conversation by one turn: append the response and its answer,
/// under `error`.
fn continued(mut messages: Vec<Value>, response: &Value, result: &str) -> Vec<Value> {
    let content = content_of(response);
    messages.push(json!({"role": "assistant", "content": content}));
    messages.push(answer(&content, result));
    messages
}

pub async fn run_claude_oauth_boundaries(
    provider: &AnthropicProvider,
    tools: &[ToolDefinition],
) -> Result<ContractReport> {
    let model = provider.model();
    let bash: Vec<ToolDefinition> = tools.iter().filter(|t| t.name == "bash").cloned().collect();
    anyhow::ensure!(!bash.is_empty(), "the tool surface has no bash tool");
    let mut probes = Probes {
        provider,
        probes: Vec::new(),
        notes: Vec::new(),
    };

    // G6.1 without thinking context.
    provider.set_reasoning_effort("low")?;
    let reply = provider
        .contract_request_body_for_doctor(
            &[Message::user("Reply with exactly: OK")],
            &bash,
            PROBE_SYSTEM,
        )
        .await?;
    let reply = with_binding(reply, "error");
    let user = user_text("Reply with exactly: OK");
    probes
        .send(
            "G6.1a",
            "a role:system message directly after a user prompt is accepted (and followed)",
            &body_with(&reply, vec![user.clone(), system_text(OPERATOR_NOTE)]),
        )
        .await?;
    probes
        .send(
            "G6.1b",
            "two adjacent role:system messages after a user prompt are accepted",
            &body_with(
                &reply,
                vec![
                    user.clone(),
                    system_text(OPERATOR_NOTE),
                    system_text("Operator note: keep the reply under ten words."),
                ],
            ),
        )
        .await?;
    probes
        .send(
            "G6.1c",
            "cache_control on a plain role:system message is accepted",
            &body_with(
                &reply,
                vec![
                    user.clone(),
                    json!({"role": "system", "content": [{
                        "type": "text", "text": OPERATOR_NOTE,
                        "cache_control": {"type": "ephemeral", "ttl": "1h"}
                    }]}),
                ],
            ),
        )
        .await?;
    probes
        .send(
            "G6.1e",
            "a role:system message whose text keeps the <system-reminder> wrapper is accepted",
            &body_with(
                &reply,
                vec![
                    user.clone(),
                    system_text(&format!(
                        "<system-reminder>\n{OPERATOR_NOTE}\n</system-reminder>"
                    )),
                ],
            ),
        )
        .await?;

    let Some(turn) = thinking_turn(&mut probes, "G6.1-T1", &bash).await? else {
        return Ok(report(model, tools, probes));
    };

    // G6.1d: a system message after the tool results, then thinking replayed
    // across it on the next request, all under `error`.
    let mut after_results = turn.messages.clone();
    after_results.push(system_text(OPERATOR_NOTE));
    let second = probes
        .send(
            "G6.1d",
            "a role:system message after a tool_result message is accepted with earlier thinking replayed under error",
            &body_with(&turn.request, after_results.clone()),
        )
        .await?;
    if second.get("content").is_some() {
        probes
            .send(
                "G6.1d-next",
                "the next request replays thinking produced before and after the system message under error",
                &body_with(
                    &turn.request,
                    continued(after_results, &second, "parity-probe 21:24"),
                ),
            )
            .await?;
    }

    // G6.2: tool changes, each appended after the tool results.
    let echo = json!({
        "name": "probe_echo",
        "description": "Echo a short text back. Used by an API compatibility probe.",
        "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}
    });
    let mut bash_changed = turn.request["tools"][0].clone();
    if let Some(object) = bash_changed.as_object_mut() {
        object.remove("cache_control");
        let description = object["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        object.insert(
            "description".to_string(),
            json!(format!("{description}\n\n(Probe: description revised.)")),
        );
    }
    let tool_change_probes: [(&'static str, &'static str, Value, Option<Value>); 4] = [
        (
            "G6.2a",
            "inline-tools: tool_addition by value of a new tool, under error",
            json!({"type": "tool_addition", "tool": {"type": "tool_definition", "definition": echo}}),
            None,
        ),
        (
            "G6.2b",
            "inline-tools: tool_addition by value replacing a same-name tool's description, under error",
            json!({"type": "tool_addition", "tool": {"type": "tool_definition", "definition": bash_changed}}),
            None,
        ),
        (
            "G6.2c",
            "tool_removal by reference of a declared tool, under error",
            json!({"type": "tool_removal", "tool": {"type": "tool_reference", "name": "bash"}}),
            None,
        ),
        (
            "G6.2d",
            "mid-conversation-tool-changes: tool_addition by reference of a tool appended to tools with defer_loading, under error",
            json!({"type": "tool_addition", "tool": {"type": "tool_reference", "name": "probe_echo"}}),
            Some({
                let mut deferred = echo.clone();
                deferred["defer_loading"] = json!(true);
                deferred
            }),
        ),
    ];
    for (id, question, block, appended_tool) in tool_change_probes {
        let mut messages = turn.messages.clone();
        messages.push(json!({"role": "system", "content": [block]}));
        let mut body = body_with(&turn.request, messages.clone());
        if let Some(tool) = appended_tool {
            body["tools"]
                .as_array_mut()
                .context("thinking turn has tools")?
                .push(tool);
        }
        let response = probes.send(id, question, &body).await?;
        if response.get("content").is_some() {
            let next_id: &'static str = match id {
                "G6.2a" => "G6.2a-next",
                "G6.2b" => "G6.2b-next",
                "G6.2c" => "G6.2c-next",
                _ => "G6.2d-next",
            };
            let mut next = body.clone();
            next["messages"] = Value::Array(continued(messages, &response, "probe-ok"));
            probes
                .send(
                    next_id,
                    "the next request after the tool change replays every thinking block under error (cache read recorded)",
                    &next,
                )
                .await?;
        }
    }

    // G6.4: modified summarized thinking in the latest assistant message.
    let mut corrupted = turn.messages.clone();
    if let Some(block) = corrupted[1]["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.iter_mut().find(|b| b["type"] == "thinking"))
    {
        let text = block["thinking"].as_str().unwrap_or_default().to_string();
        if text.is_empty() {
            probes.notes.push(
                "G6.4: the thinking turn returned empty summarized text; nothing to modify"
                    .to_string(),
            );
        } else {
            let mut chars: Vec<char> = text.chars().collect();
            let middle = chars.len() / 2;
            chars[middle] = '\u{FFFD}';
            block["thinking"] = json!(chars.into_iter().collect::<String>());
            probes
                .send(
                    "G6.4",
                    "a replayed thinking block whose summarized text has one character replaced by U+FFFD, under drop_block",
                    &with_binding(body_with(&turn.request, corrupted), "drop_block"),
                )
                .await?;
        }
    }

    // G6.6: the billing-header version bumped.
    let mut bumped = body_with(&turn.request, turn.messages.clone());
    if let Some(block) = bumped["system"]
        .as_array_mut()
        .and_then(|blocks| blocks.first_mut())
    {
        let text = block["text"].as_str().unwrap_or_default().to_string();
        let version = text
            .split("cc_version=")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .context("the first system block is not the OAuth billing header")?;
        let changed = text.replace(&format!("cc_version={version}"), "cc_version=9.9.999");
        block["text"] = json!(changed);
        probes
            .send(
                "G6.6",
                "the same conversation with the OAuth billing-header version changed, under error",
                &bumped,
            )
            .await?;
    }

    if model == "claude-opus-5-5" {
        switch_and_back(&mut probes, &turn, &model).await?;
    }
    Ok(report(model, tools, probes))
}

/// G6.5: Opus 5.5, two tool turns on Sonnet 5.5, back to Opus 5.5, under
/// `error`. Each request is built by the production builder for the model it
/// goes to, then given the conversation so far.
async fn switch_and_back(
    probes: &mut Probes<'_>,
    turn: &ThinkingTurn,
    home_model: &str,
) -> Result<()> {
    let tools = turn.request["tools"].clone();
    let body_for = |probes: &Probes<'_>, model: &str| -> Result<()> {
        probes.provider.set_model(model)?;
        probes.provider.set_reasoning_effort("high")?;
        Ok(())
    };
    let mut messages = turn.messages.clone();
    let legs: [(&'static str, &str, &str); 4] = [
        ("G6.5-sonnet-1", SWITCH_SECOND_MODEL, SWITCH_STEPS[0]),
        ("G6.5-sonnet-2", SWITCH_SECOND_MODEL, SWITCH_STEPS[1]),
        ("G6.5-opus-back", home_model, SWITCH_STEPS[2]),
        ("G6.5-opus-next", home_model, SWITCH_STEPS[3]),
    ];
    for (id, model, step) in legs {
        body_for(probes, model)?;
        let mut body = with_binding(
            probes
                .provider
                .contract_request_body_for_doctor(&[Message::user("x")], &[], PROBE_SYSTEM)
                .await?,
            "error",
        );
        body["tools"] = tools.clone();
        body["tool_choice"] = turn.request["tool_choice"].clone();
        // The step's prompt joins the previous answer, as a follow-up turn.
        if let Some(last) = messages
            .last_mut()
            .and_then(|m| m["content"].as_array_mut())
        {
            last.push(json!({"type": "text", "text": step}));
        }
        body["messages"] = Value::Array(messages.clone());
        let response = probes
            .send(
                id,
                "model switch leg under error: every input_transformations entry is recorded",
                &body,
            )
            .await?;
        if response.get("content").is_none() {
            probes
                .notes
                .push(format!("{id} failed; the rest of G6.5 did not run"));
            break;
        }
        messages = continued(messages, &response, "ok");
    }
    probes.provider.set_model(home_model)?;
    Ok(())
}

fn report(model: String, tools: &[ToolDefinition], probes: Probes<'_>) -> ContractReport {
    ContractReport {
        contract: CLAUDE_OAUTH_BOUNDARY_CONTRACT,
        route: "claude-oauth (native Anthropic runtime)",
        model,
        date: chrono::Utc::now().to_rfc3339(),
        tool_count: tools.len(),
        tools: tools.iter().map(|t| t.name.clone()).collect(),
        strict_candidate_tools: Vec::new(),
        probes: probes.probes,
        notes: probes.notes,
    }
}
