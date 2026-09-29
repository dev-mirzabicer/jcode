//! Live Claude OAuth provider-contract probe.
//!
//! `jcode provider-doctor claude --contract claude-oauth --model <id>` runs a
//! fixed set of credentialed Messages requests that establish what the Claude
//! OAuth route accepts. It reproduces the INT-01 Gate 0 probes (2026-09-29)
//! through jcode's own request builder, token resolution and attribution
//! headers, and compares each outcome with the Gate 0 observation so that a
//! provider change shows up as recorded drift rather than as a field failure.
//!
//! Every request spends subscription quota (roughly seventeen requests, a few
//! of them carrying the complete tool surface). The credential never leaves
//! the runtime; the returned report carries responses with signatures
//! replaced by their length.

use anyhow::{Context, Result};
use jcode_base::message::{Message, ToolDefinition};
use jcode_base::provider::Provider;
use jcode_provider_anthropic_runtime::AnthropicProvider;
use jcode_provider_core::openai_schema::{
    openai_compatible_schema, schema_supports_strict, strict_normalize_schema,
};
use serde::Serialize;
use serde_json::{Value, json};

pub const CLAUDE_OAUTH_CONTRACT: &str = "claude-oauth";

/// Models Gate 0 measured at all. Others have no recorded expectation.
const GATE0_MEASURED_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
    "claude-sonnet-5",
];

const PROBE_SYSTEM: &str = "You are a probe assistant for an API compatibility test. Follow the user's instructions exactly and keep answers minimal.";

/// A second step that needs fresh reasoning and another tool call, so the
/// conversation holds two turns of signed thinking (probe `T8`).
const FOLLOW_UP_TASK: &str = "Now think it through again: after arriving, the train continues 97 km at 83 km/h. Compute the new exact arrival time (HH:MM, rounded down), then use the bash tool to run exactly `echo parity-probe-2 <HH:MM>` with your answer.";

const THINKING_TASK: &str = "Think it through step by step before acting: a train leaves at 14:37 and travels 283 km at 91 km/h, then waits 17 minutes, then travels 146 km at 73 km/h. Compute the exact arrival time (HH:MM, rounded down), then use the bash tool to run exactly `echo parity-probe <HH:MM>` with your answer. After you see the result, reply with exactly: DONE";

/// What Gate 0 observed for a probe on the model under test.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Gate0Expectation {
    /// HTTP 200 with no `input_transformations` entry.
    Accepted,
    /// HTTP 400.
    Rejected,
    /// HTTP 200 with a `prefix_binding_mismatch` thinking drop reported.
    ThinkingDropped,
    /// Gate 0 did not settle this; the result decides a WP-01 question.
    Undetermined,
}

#[derive(Debug, Serialize)]
pub struct ContractProbe {
    pub id: &'static str,
    pub question: &'static str,
    pub gate0: Gate0Expectation,
    pub status: u16,
    pub observed: Gate0Expectation,
    /// `None` when Gate 0 has no expectation for this probe and model.
    pub agrees_with_gate0: Option<bool>,
    pub summary: String,
    pub response: Value,
}

#[derive(Debug, Serialize)]
pub struct ContractReport {
    pub contract: &'static str,
    pub route: &'static str,
    pub model: String,
    pub date: String,
    pub tool_count: usize,
    pub tools: Vec<String>,
    pub strict_candidate_tools: Vec<String>,
    pub probes: Vec<ContractProbe>,
    /// Probes that could not run because the model did not produce the turn
    /// they need.
    pub notes: Vec<String>,
}

impl ContractReport {
    pub fn drift(&self) -> Vec<&ContractProbe> {
        self.probes
            .iter()
            .filter(|probe| probe.agrees_with_gate0 == Some(false))
            .collect()
    }

    pub fn probe(&self, id: &str) -> Option<&ContractProbe> {
        self.probes.iter().find(|probe| probe.id == id)
    }
}

struct Runner<'a> {
    provider: &'a AnthropicProvider,
    prefix_bound: bool,
    measured: bool,
    probes: Vec<ContractProbe>,
}

/// A probe's Gate 0 expectation, possibly depending on whether the model binds
/// signed thinking to its prefix.
#[derive(Clone, Copy)]
enum Expect {
    Always(Gate0Expectation),
    /// Prefix-bound models show this outcome; other models accept the request.
    IfPrefixBound(Gate0Expectation),
}

fn resolve_expectation(expect: Expect, prefix_bound: bool) -> Gate0Expectation {
    match expect {
        Expect::Always(outcome) => outcome,
        Expect::IfPrefixBound(outcome) if prefix_bound => outcome,
        Expect::IfPrefixBound(_) => Gate0Expectation::Accepted,
    }
}

impl Runner<'_> {
    async fn send(
        &mut self,
        id: &'static str,
        question: &'static str,
        gate0: Expect,
        body: &Value,
    ) -> Result<Value> {
        let (status, response) = self
            .provider
            .send_contract_request_for_doctor(body)
            .await
            .with_context(|| format!("probe {id}"))?;
        let observed = classify(status, &response);
        let gate0 = resolve_expectation(gate0, self.prefix_bound);
        let agrees_with_gate0 =
            (self.measured && gate0 != Gate0Expectation::Undetermined).then_some(observed == gate0);
        self.probes.push(ContractProbe {
            id,
            question,
            gate0,
            status,
            observed,
            agrees_with_gate0,
            summary: summarize(&response),
            response: redact(&response),
        });
        Ok(response)
    }
}

fn classify(status: u16, response: &Value) -> Gate0Expectation {
    if status != 200 {
        return if status == 400 {
            Gate0Expectation::Rejected
        } else {
            Gate0Expectation::Undetermined
        };
    }
    let dropped = response
        .get("input_transformations")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item["type"] == "thinking_dropped" && item["reason"] == "prefix_binding_mismatch"
            })
        });
    if dropped {
        Gate0Expectation::ThinkingDropped
    } else {
        Gate0Expectation::Accepted
    }
}

/// Run the contract probe for the provider's current model over the given
/// tool surface. The provider must already be pinned to the OAuth route.
pub async fn run_claude_oauth_contract(
    provider: &AnthropicProvider,
    tools: &[ToolDefinition],
) -> Result<ContractReport> {
    let model = provider.model();
    let mut runner = Runner {
        provider,
        // The runtime's per-model binding policy (INT-01 D11) decides which
        // outcomes the binding probes expect.
        prefix_bound: jcode_provider_core::anthropic_reasoning_binding(&model)
            == jcode_provider_core::ReasoningBinding::PrefixBound,
        measured: GATE0_MEASURED_MODELS.contains(&model.as_str()),
        probes: Vec::new(),
    };
    let reply_ok = [Message::user("Reply with exactly: OK")];

    provider.set_reasoning_effort("low")?;
    let surface = provider
        .contract_request_body_for_doctor(&reply_ok, tools, PROBE_SYSTEM)
        .await?;
    runner
        .send(
            "surface",
            "G0.1/G0.6/R02: the production request (every tool under its registry name, full schemas, parallel calls disabled) is accepted",
            Expect::Always(Gate0Expectation::Accepted),
            &surface,
        )
        .await?;

    let bash: Vec<ToolDefinition> = tools.iter().filter(|t| t.name == "bash").cloned().collect();
    anyhow::ensure!(!bash.is_empty(), "the tool surface has no bash tool");
    let bash_body = provider
        .contract_request_body_for_doctor(&reply_ok, &bash, PROBE_SYSTEM)
        .await?;

    let mut raw_strict = bash_body.clone();
    raw_strict["tools"][0]["strict"] = json!(true);
    runner
        .send(
            "strict_raw",
            "G0.7: strict:true on the bash tool's Anthropic schema (no strict normalization) is rejected",
            Expect::Always(Gate0Expectation::Rejected),
            &raw_strict,
        )
        .await?;

    let (strict_surface, strict_candidate_tools) = strict_candidate(&surface, tools);
    runner
        .send(
            "strict_normalized",
            "R04: strict:true with the shared strict normalizer on exactly the tools GPT marks strict",
            Expect::Always(Gate0Expectation::Undetermined),
            &strict_surface,
        )
        .await?;

    let mut binding = bash_body.clone();
    binding["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior": "drop_block"});
    runner
        .send(
            "binding_drop_block",
            "G0.2: thinking.block_binding with the binding-controls beta is accepted",
            Expect::Always(Gate0Expectation::Accepted),
            &binding,
        )
        .await?;

    let mut sampled = bash_body.clone();
    if let Some(object) = sampled.as_object_mut() {
        object.remove("thinking");
        object.remove("output_config");
        object.insert("temperature".to_string(), json!(1.0));
    }
    runner
        .send(
            "temperature_without_thinking",
            "G0.5: temperature 1.0 with thinking omitted is tolerated",
            Expect::Always(Gate0Expectation::Accepted),
            &sampled,
        )
        .await?;

    provider.set_reasoning_effort("high")?;
    let task = [Message::user(THINKING_TASK)];
    let t1 = provider
        .contract_request_body_for_doctor(&task, &bash, PROBE_SYSTEM)
        .await?;
    let first = runner
        .send(
            "T1",
            "an assistant turn with signed thinking before a tool_use is produced",
            Expect::Always(Gate0Expectation::Accepted),
            &t1,
        )
        .await?;
    let assistant = first["content"].clone();
    let tool_id = assistant
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use"))
        .and_then(|block| block["id"].as_str())
        .map(str::to_string);
    let has_thinking = assistant
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "thinking"));
    let mut notes = Vec::new();
    if let (Some(tool_id), true) = (tool_id, has_thinking) {
        replay_probes(&mut runner, &t1, &assistant, &tool_id, &mut notes).await?;
    } else {
        notes.push(
            "T1 produced no signed thinking before a tool_use; T2-T8 did not run".to_string(),
        );
    }

    Ok(ContractReport {
        contract: CLAUDE_OAUTH_CONTRACT,
        route: "claude-oauth (native Anthropic runtime)",
        model,
        date: chrono::Utc::now().to_rfc3339(),
        tool_count: tools.len(),
        tools: tools.iter().map(|t| t.name.clone()).collect(),
        strict_candidate_tools,
        probes: runner.probes,
        notes,
    })
}

/// Capture raw SSE responses for the runtime's reasoning-capture fixtures
/// (INT-01/WP-02): the T1 thinking task with readable summaries, and again
/// with the model's default `display: "omitted"`, where signed blocks carry
/// empty text, and the next turn after the tool result. Each file holds only
/// what the provider streamed; signatures are kept as opaque test data. Spends
/// three requests.
pub async fn capture_claude_sse_fixtures(
    provider: &AnthropicProvider,
    tools: &[ToolDefinition],
    dir: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>> {
    let bash: Vec<ToolDefinition> = tools.iter().filter(|t| t.name == "bash").cloned().collect();
    anyhow::ensure!(!bash.is_empty(), "the tool surface has no bash tool");
    provider.set_reasoning_effort("high")?;
    let summarized = provider
        .contract_request_body_for_doctor(&[Message::user(THINKING_TASK)], &bash, PROBE_SYSTEM)
        .await?;
    let mut omitted = summarized.clone();
    if let Some(thinking) = omitted.get_mut("thinking").and_then(Value::as_object_mut) {
        thinking.remove("display");
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let model = provider.model();
    let mut written = Vec::new();
    let mut summarized_sse = String::new();
    for (name, body) in [("summarized", &summarized), ("omitted", &omitted)] {
        let (status, sse) = provider.stream_contract_request_for_doctor(body).await?;
        anyhow::ensure!(
            status == 200,
            "capture `{name}` returned HTTP {status}: {sse}"
        );
        let path = dir.join(format!("{model}-thinking-{name}.sse"));
        std::fs::write(&path, &sse).with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
        if name == "summarized" {
            summarized_sse = sse;
        }
    }

    // The next turn, after the tool result: it replays the first turn exactly
    // as streamed, under `prefix_mismatch_behavior: "error"`, so acceptance
    // shows the replay is byte-exact. Between tool calls, current models
    // return progress notes as further signed thinking blocks.
    let assistant = assistant_content_from_sse(&summarized_sse)?;
    let tool_id = assistant
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use"))
        .and_then(|block| block["id"].as_str())
        .context("the captured turn has no tool_use")?
        .to_string();
    let mut follow_up = summarized.clone();
    follow_up["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior": "error"});
    follow_up["messages"] = json!([
        {"role": "user", "content": [{"type": "text", "text": THINKING_TASK}]},
        {"role": "assistant", "content": assistant},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": tool_id, "content": "parity-probe"},
            {"type": "text", "text": FOLLOW_UP_TASK}
        ]}
    ]);
    let (status, sse) = provider
        .stream_contract_request_for_doctor(&follow_up)
        .await?;
    anyhow::ensure!(
        status == 200,
        "capture `follow-up` returned HTTP {status}: {sse}"
    );
    let path = dir.join(format!("{model}-thinking-follow-up.sse"));
    std::fs::write(&path, sse).with_context(|| format!("writing {}", path.display()))?;
    written.push(path);
    Ok(written)
}

/// Rebuild an assistant turn's content from its SSE stream by concatenating
/// each block's deltas, independently of the runtime's parser.
fn assistant_content_from_sse(sse: &str) -> Result<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    let mut tool_inputs: Vec<String> = Vec::new();
    for line in sse.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let event: Value = serde_json::from_str(data.trim())?;
        let index = event["index"].as_u64().unwrap_or_default() as usize;
        match event["type"].as_str() {
            Some("content_block_start") => {
                let mut block = event["content_block"].clone();
                if let Some(object) = block.as_object_mut() {
                    object.remove("caller");
                }
                blocks.resize(index + 1, Value::Null);
                tool_inputs.resize(index + 1, String::new());
                blocks[index] = block;
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let block = &mut blocks[index];
                match delta["type"].as_str() {
                    Some("thinking_delta") => append(block, "thinking", &delta["thinking"]),
                    Some("signature_delta") => append(block, "signature", &delta["signature"]),
                    Some("text_delta") => append(block, "text", &delta["text"]),
                    Some("input_json_delta") => {
                        tool_inputs[index].push_str(delta["partial_json"].as_str().unwrap_or(""))
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") if blocks[index]["type"] == "tool_use" => {
                let input = &tool_inputs[index];
                blocks[index]["input"] = if input.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(input)?
                };
            }
            _ => {}
        }
    }
    Ok(Value::Array(blocks))
}

fn append(block: &mut Value, field: &str, text: &Value) {
    let mut current = block[field].as_str().unwrap_or_default().to_string();
    current.push_str(text.as_str().unwrap_or_default());
    block[field] = json!(current);
}

/// The T1 body with `thinking.block_binding` removed. Production requests for
/// prefix-bound models carry the control; the Gate 0 probes that ask what
/// happens without it must not.
fn without_binding(mut body: Value) -> Value {
    if let Some(thinking) = body.get_mut("thinking").and_then(Value::as_object_mut) {
        thinking.remove("block_binding");
    }
    body
}

fn thinking_blocks(content: &Value) -> usize {
    content
        .as_array()
        .map(|blocks| blocks.iter().filter(|b| b["type"] == "thinking").count())
        .unwrap_or(0)
}

async fn replay_probes(
    runner: &mut Runner<'_>,
    t1: &Value,
    assistant: &Value,
    tool_id: &str,
    notes: &mut Vec<String>,
) -> Result<()> {
    let replay = |user_text: &str, assistant: &Value| {
        let mut body = t1.clone();
        body["messages"] = json!([
            {"role": "user", "content": [{"type": "text", "text": user_text}]},
            {"role": "assistant", "content": assistant},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_id, "content": "parity-probe"}]}
        ]);
        body
    };
    let with_binding = |mut body: Value, behavior: &str| {
        body["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior": behavior});
        body
    };
    let edited = format!("{THINKING_TASK} Thanks.");

    runner
        .send(
            "T2",
            "unchanged replay of signed thinking is accepted",
            Expect::Always(Gate0Expectation::Accepted),
            &replay(THINKING_TASK, assistant),
        )
        .await?;
    runner
        .send(
            "T3",
            "an edited earlier message without a binding field is silently accepted (account not enforced)",
            Expect::Always(Gate0Expectation::Accepted),
            &without_binding(replay(&edited, assistant)),
        )
        .await?;
    runner
        .send(
            "T3b",
            "the same edit with prefix_mismatch_behavior=error is rejected on prefix-bound models",
            Expect::IfPrefixBound(Gate0Expectation::Rejected),
            &with_binding(replay(&edited, assistant), "error"),
        )
        .await?;
    runner
        .send(
            "T3c",
            "the same edit with drop_block reports the dropped thinking on prefix-bound models",
            Expect::IfPrefixBound(Gate0Expectation::ThinkingDropped),
            &with_binding(replay(&edited, assistant), "drop_block"),
        )
        .await?;

    let mut changed_system = with_binding(replay(THINKING_TASK, assistant), "drop_block");
    if let Some(last) = changed_system["system"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
    {
        let text = last["text"].as_str().unwrap_or_default().to_string();
        last["text"] = json!(format!(
            "{text}\n\n# System Reminder\n\nper-turn reminder changed"
        ));
    }
    runner
        .send(
            "T7",
            "a per-turn change appended to system invalidates earlier thinking on prefix-bound models",
            Expect::IfPrefixBound(Gate0Expectation::ThinkingDropped),
            &changed_system,
        )
        .await?;

    let mut spliced = assistant.clone();
    for block in spliced.as_array_mut().into_iter().flatten() {
        if block["type"] == "thinking" {
            let signature = block["signature"].as_str().unwrap_or_default().to_string();
            block["signature"] = json!(format!("{signature}{signature}"));
        }
    }
    runner
        .send(
            "T4",
            "a doubled (spliced) signature is not rejected",
            Expect::Always(Gate0Expectation::Accepted),
            &without_binding(replay(THINKING_TASK, &spliced)),
        )
        .await?;

    let blocks = assistant.as_array().cloned().unwrap_or_default();
    let (thinking, rest): (Vec<Value>, Vec<Value>) =
        blocks.into_iter().partition(|b| b["type"] == "thinking");
    let reordered: Vec<Value> = rest.iter().cloned().chain(thinking).collect();
    runner
        .send(
            "T5",
            "thinking moved after tool_use is not rejected",
            Expect::Always(Gate0Expectation::Accepted),
            &without_binding(replay(THINKING_TASK, &json!(reordered))),
        )
        .await?;
    runner
        .send(
            "T6",
            "stripping every thinking block is accepted",
            Expect::Always(Gate0Expectation::Accepted),
            &without_binding(replay(THINKING_TASK, &json!(rest))),
        )
        .await?;

    // T8: jcode treats a replayed block as valid when the thinking replayed
    // before it is its recorded predecessor or it is the first one replayed
    // (INT-01/WP-02, from the documented preserved-thinking rule). This checks
    // the permissive half live: strip the first turn's thinking and replay the
    // second turn's under `error`.
    let mut follow_up = t1.clone();
    follow_up["messages"] = json!([
        {"role": "user", "content": [{"type": "text", "text": THINKING_TASK}]},
        {"role": "assistant", "content": assistant},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": tool_id, "content": "parity-probe"},
            {"type": "text", "text": FOLLOW_UP_TASK}
        ]}
    ]);
    let second = runner
        .send(
            "T8a",
            "a second signed-thinking turn over an unchanged history is accepted",
            Expect::Always(Gate0Expectation::Accepted),
            &follow_up,
        )
        .await?;
    let second_assistant = second["content"].clone();
    let second_tool_id = second_assistant
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use"))
        .and_then(|block| block["id"].as_str())
        .map(str::to_string);
    let Some(second_tool_id) = second_tool_id.filter(|_| thinking_blocks(&second_assistant) > 0)
    else {
        notes.push("T8a produced no signed thinking before a tool_use; T8 did not run".to_string());
        return Ok(());
    };
    let mut leading_stripped = with_binding(follow_up.clone(), "error");
    leading_stripped["messages"][1]["content"] = json!(rest);
    if let Some(messages) = leading_stripped["messages"].as_array_mut() {
        messages.push(json!({"role": "assistant", "content": second_assistant}));
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": second_tool_id, "content": "parity-probe-2"}
        ]}));
    }
    runner
        .send(
            "T8",
            "stripping the first turn's thinking (a leading run) keeps the second turn's thinking valid under prefix_mismatch_behavior=error",
            Expect::Always(Gate0Expectation::Accepted),
            &leading_stripped,
        )
        .await?;
    Ok(())
}

/// The production surface with `strict: true` and the shared strict
/// normalizer applied to exactly the tools GPT marks strict.
fn strict_candidate(surface: &Value, tools: &[ToolDefinition]) -> (Value, Vec<String>) {
    let mut body = surface.clone();
    let mut names = Vec::new();
    if let Some(wire) = body["tools"].as_array_mut() {
        for (definition, tool) in tools.iter().zip(wire.iter_mut()) {
            if schema_supports_strict(&openai_compatible_schema(&definition.input_schema)) {
                tool["input_schema"] = strict_normalize_schema(&tool["input_schema"]);
                tool["strict"] = json!(true);
                names.push(definition.name.clone());
            }
        }
    }
    (body, names)
}

fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if key == "signature" || key == "data" {
                        let length = value.as_str().map(str::len).unwrap_or(0);
                        (key.clone(), json!(format!("<{length} chars>")))
                    } else {
                        (key.clone(), redact(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

fn summarize(response: &Value) -> String {
    if let Some(error) = response.get("error") {
        return format!("error {error}");
    }
    let blocks: Vec<String> = response
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|block| match block["type"].as_str().unwrap_or("?") {
            "thinking" => format!(
                "thinking(text={},sig={})",
                block["thinking"].as_str().map(str::len).unwrap_or(0),
                block["signature"].as_str().map(str::len).unwrap_or(0)
            ),
            "tool_use" => format!("tool_use({})", block["name"].as_str().unwrap_or("?")),
            other => other.to_string(),
        })
        .collect();
    let usage = &response["usage"];
    format!(
        "stop={} blocks=[{}] in={} cache_read={} cache_write={} input_transformations={}",
        response["stop_reason"],
        blocks.join(", "),
        usage["input_tokens"],
        usage["cache_read_input_tokens"],
        usage["cache_creation_input_tokens"],
        response
            .get("input_transformations")
            .map(Value::to_string)
            .unwrap_or_else(|| "absent".to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_classified_from_status_and_transformations() {
        assert_eq!(classify(200, &json!({})), Gate0Expectation::Accepted);
        assert_eq!(
            classify(200, &json!({"input_transformations": []})),
            Gate0Expectation::Accepted
        );
        assert_eq!(
            classify(
                200,
                &json!({"input_transformations": [{"type": "thinking_dropped", "reason": "prefix_binding_mismatch"}]})
            ),
            Gate0Expectation::ThinkingDropped
        );
        assert_eq!(classify(400, &json!({})), Gate0Expectation::Rejected);
        assert_eq!(classify(429, &json!({})), Gate0Expectation::Undetermined);
    }

    #[test]
    fn only_binding_probes_depend_on_the_prefix_bound_model_set() {
        let rejected = Gate0Expectation::Rejected;
        for bound in [true, false] {
            assert_eq!(
                resolve_expectation(Expect::Always(rejected), bound),
                rejected
            );
        }
        assert_eq!(
            resolve_expectation(Expect::IfPrefixBound(rejected), true),
            rejected
        );
        assert_eq!(
            resolve_expectation(Expect::IfPrefixBound(rejected), false),
            Gate0Expectation::Accepted
        );
    }

    #[test]
    fn signatures_and_redacted_data_never_reach_the_artifact() {
        let redacted = redact(&json!({"content": [
            {"type": "thinking", "thinking": "t", "signature": "secret-sig"},
            {"type": "redacted_thinking", "data": "opaque"}
        ]}));
        let text = redacted.to_string();
        assert!(
            !text.contains("secret-sig") && !text.contains("opaque"),
            "{text}"
        );
        assert!(text.contains("<10 chars>"), "{text}");
    }

    #[test]
    fn strict_candidates_follow_the_gpt_eligibility_predicate() {
        let tools = vec![
            ToolDefinition {
                name: "closed".into(),
                description: String::new(),
                input_schema: json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}),
            },
            ToolDefinition {
                name: "open".into(),
                description: String::new(),
                input_schema: json!({"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":true}),
            },
        ];
        let surface = json!({"tools": [
            {"name": "closed", "input_schema": tools[0].input_schema},
            {"name": "open", "input_schema": tools[1].input_schema}
        ]});
        let (body, names) = strict_candidate(&surface, &tools);
        assert_eq!(names, vec!["closed"]);
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(
            body["tools"][0]["input_schema"]["additionalProperties"],
            false
        );
        assert!(body["tools"][1].get("strict").is_none());
    }
}
