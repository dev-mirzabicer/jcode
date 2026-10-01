//! Boundary behavior of the Anthropic request path, through a local HTTP
//! server (INT-01/WP-06 R17, R19, R20): real sockets, real response bodies cut
//! at arbitrary bytes, and the production `complete` entry points.

use super::*;
use futures::StreamExt;
use jcode_provider_core::{ProviderRequestContext, ProviderRequestReplan};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One scripted HTTP response.
#[derive(Clone)]
struct Scripted {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// The body is written in pieces of this many bytes, each flushed.
    piece: usize,
}

impl Scripted {
    fn sse(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: body.into(),
            piece: usize::MAX,
        }
    }

    fn in_pieces(mut self, piece: usize) -> Self {
        self.piece = piece;
        self
    }

    fn error(status: u16, kind: &str, message: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: json!({"type": "error", "error": {"type": kind, "message": message}})
                .to_string()
                .into_bytes(),
            piece: usize::MAX,
        }
    }
}

/// A local Messages endpoint that answers each request with the next
/// scripted response (repeating the last) and records every request body.
/// Bodies have no length header: the connection closing is a clean end of
/// body, exactly what a proxy that stops early produces.
struct Fixture {
    base: String,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn serve(script: Vec<Scripted>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut served = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let response = script[served.min(script.len() - 1)].clone();
                served += 1;
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let body_start = loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buffer[..body_start]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buffer.len() < body_start + length {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    if let Ok(body) = serde_json::from_slice::<Value>(&buffer[body_start..]) {
                        recorded.lock().unwrap().push(body);
                    }
                    let head = format!(
                        "HTTP/1.1 {} X\r\ncontent-type: {}\r\nconnection: close\r\n\r\n",
                        response.status, response.content_type
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    for piece in response
                        .body
                        .chunks(response.piece.min(response.body.len().max(1)))
                    {
                        if socket.write_all(piece).await.is_err() {
                            return;
                        }
                        let _ = socket.flush().await;
                        if response.piece != usize::MAX {
                            tokio::task::yield_now().await;
                        }
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => jcode_base::env::set_var(self.key, value),
            None => jcode_base::env::remove_var(self.key),
        }
    }
}

/// An API-key provider on `model` pointed at the fixture, with a private
/// home so no real credential or catalog is read. Hold the returned guards.
struct TestProvider {
    provider: AnthropicProvider,
    _guards: (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        Vec<EnvGuard>,
    ),
}

fn provider_for(fixture: &Fixture, model: &str) -> TestProvider {
    let lock = jcode_base::storage::lock_test_env();
    let home = tempfile::TempDir::new().unwrap();
    let guards = vec![
        EnvGuard::set("JCODE_HOME", home.path()),
        EnvGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key"),
        EnvGuard::set("JCODE_RUNTIME_PROVIDER", "claude-api"),
    ];
    let mut provider = AnthropicProvider::new();
    provider.api_base = Arc::from(fixture.base.as_str());
    *provider.model.write().unwrap() = model.to_string();
    *provider.reasoning_effort.write().unwrap() = None;
    TestProvider {
        provider,
        _guards: (lock, home, guards),
    }
}

fn tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {"content": {"type": "string"}},
            "required": ["content"]
        }),
    }]
}

fn replanning() -> ProviderRequestContext {
    ProviderRequestContext {
        caller_replans: true,
        ..ProviderRequestContext::default()
    }
}

async fn collect(mut stream: EventStream) -> Vec<Result<StreamEvent>> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn frame(events: &[Value], line_end: &str) -> String {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}{line_end}data: {}{line_end}{line_end}",
                event["type"].as_str().unwrap(),
                event
            )
        })
        .collect()
}

const THINKING: &str = "Check the edge case — then write café for 漢字 🦀.";
const SIGNATURE: &str = "c2lnbmF0dXJl4oCUw6k=";
const TOOL_INPUT: &str = "{\"content\":\"naïve résumé — 漢字\"}";
const TEXT: &str = "Écrit: “naïve” ✓";

/// A complete response: signed thinking, text and a tool call, every string
/// carrying multi-byte characters.
fn response_events(transformations: Option<Value>) -> Vec<Value> {
    let mut message = json!({"model": "claude-opus-5-5", "usage": {"input_tokens": 10}});
    if let Some(transformations) = &transformations {
        message["input_transformations"] = transformations.clone();
    }
    vec![
        json!({"type": "message_start", "message": message}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": THINKING}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": SIGNATURE}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": TEXT}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "write", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": TOOL_INPUT}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ]
}

/// What the consumer received: thinking text and signature, visible text and
/// tool input, and whether the provider ended the message.
#[derive(Debug, Default, PartialEq, Eq)]
struct Received {
    thinking: String,
    signature: String,
    text: String,
    tool_input: String,
    ended: bool,
    rollbacks: usize,
}

fn received(events: &[Result<StreamEvent>]) -> Received {
    let mut out = Received::default();
    for event in events.iter().flatten() {
        match event {
            StreamEvent::RetryRollback { .. } => {
                out = Received {
                    rollbacks: out.rollbacks + 1,
                    ..Received::default()
                };
            }
            StreamEvent::ReplayableReasoning(ReplayableReasoningBlock::AnthropicThinking {
                thinking,
                signature,
                ..
            }) => {
                out.thinking = thinking.clone();
                out.signature = signature.clone();
            }
            StreamEvent::TextDelta(text) => out.text.push_str(text),
            StreamEvent::ToolInputDelta(delta) => out.tool_input.push_str(delta),
            StreamEvent::MessageEnd { .. } => out.ended = true,
            _ => {}
        }
    }
    out
}

fn complete_received() -> Received {
    Received {
        thinking: THINKING.to_string(),
        signature: SIGNATURE.to_string(),
        text: TEXT.to_string(),
        tool_input: TOOL_INPUT.to_string(),
        ended: true,
        rollbacks: 0,
    }
}

#[tokio::test]
async fn multibyte_text_survives_any_chunking_and_both_line_endings_over_http() {
    for line_end in ["\n", "\r\n"] {
        let body = frame(&response_events(None), line_end);
        for piece in [1, 2, 3, 5, 7, 61, usize::MAX] {
            let fixture = Fixture::serve(vec![Scripted::sse(body.clone()).in_pieces(piece)]).await;
            let test = provider_for(&fixture, "claude-opus-5-5");
            let events = collect(
                test.provider
                    .complete(&[Message::user("go")], &tools(), "system", None)
                    .await
                    .unwrap(),
            )
            .await;
            assert!(
                events.iter().all(Result::is_ok),
                "line end {line_end:?} piece {piece}: {events:?}"
            );
            assert_eq!(
                received(&events),
                complete_received(),
                "line end {line_end:?} piece {piece}"
            );
        }
    }
}

/// One attempt over HTTP, without the retry loop's delays.
async fn one_attempt(test: &TestProvider) -> (Result<()>, Vec<StreamEvent>) {
    let request = test.provider.build_api_request(
        "claude-opus-5-5",
        &[Message::user("go")],
        &tools(),
        build_system_param("system", false),
        false,
    );
    let binding = jcode_provider_anthropic::binding::analyze_request(&request).binding;
    let (tx, mut rx) = mpsc::channel(256);
    let result = stream_response(
        test.provider.client.clone(),
        &test.provider.route(false),
        "key".to_string(),
        request.clone(),
        SseStreamState::new(&request.model, binding),
        tx,
        "claude-opus-5-5",
    )
    .await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event.unwrap());
    }
    (result, events)
}

#[tokio::test]
async fn a_body_that_ends_before_the_response_completes_is_a_transport_fault() {
    let events = response_events(None);
    // The body ends cleanly after each event in turn: after every event type,
    // and inside the thinking, text and tool blocks.
    for cut in 1..events.len() {
        let fixture = Fixture::serve(vec![Scripted::sse(frame(&events[..cut], "\n"))]).await;
        let test = provider_for(&fixture, "claude-opus-5-5");
        let (result, _) = one_attempt(&test).await;
        let error = format!("{:#}", result.expect_err("a cut-off body never completes"));
        assert!(
            error.contains(STREAM_INCOMPLETE),
            "cut after {}: {error}",
            events[cut - 1]["type"]
        );
        assert!(
            is_retryable_error(&error.to_lowercase()),
            "an incomplete body is retried like a transport fault: {error}"
        );
    }
    // The body ends inside an event, and inside a JSON string.
    let whole = frame(&events, "\n");
    for cut in [whole.len() - 9, whole.find("café").unwrap() + 4] {
        let fixture = Fixture::serve(vec![Scripted::sse(&whole.as_bytes()[..cut])]).await;
        let test = provider_for(&fixture, "claude-opus-5-5");
        let (result, _) = one_attempt(&test).await;
        let error = format!("{:#}", result.expect_err("a cut-off body never completes"));
        assert!(error.contains(STREAM_INCOMPLETE), "{error}");
    }
    // Trailing whitespace after `message_stop` is not leftover data.
    let fixture = Fixture::serve(vec![Scripted::sse(format!("{whole}\n \n"))]).await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let (result, events) = one_attempt(&test).await;
    result.expect("a complete body with trailing blank lines");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageEnd { .. }))
    );
}

#[tokio::test]
async fn an_incomplete_response_is_retried_and_partial_output_is_rolled_back() {
    let events = response_events(None);
    let whole = frame(&events, "\n");
    // Before any output: the retry is invisible.
    let fixture = Fixture::serve(vec![
        Scripted::sse(frame(&events[..1], "\n")),
        Scripted::sse(whole.clone()),
    ])
    .await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let received_events = collect(
        test.provider
            .complete(&[Message::user("go")], &tools(), "system", None)
            .await
            .unwrap(),
    )
    .await;
    assert!(
        received_events.iter().all(Result::is_ok),
        "{received_events:?}"
    );
    assert_eq!(received(&received_events), complete_received());
    assert_eq!(fixture.requests().len(), 2);
    drop(test);

    // After output began (the text block was streamed, the tool call was
    // not): the partial attempt is rolled back and the response replays.
    let fixture = Fixture::serve(vec![
        Scripted::sse(frame(&events[..9], "\n")),
        Scripted::sse(whole.clone()),
    ])
    .await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let received_events = collect(
        test.provider
            .complete(&[Message::user("go")], &tools(), "system", None)
            .await
            .unwrap(),
    )
    .await;
    assert!(
        received_events.iter().all(Result::is_ok),
        "{received_events:?}"
    );
    assert_eq!(
        received(&received_events),
        Received {
            rollbacks: 1,
            ..complete_received()
        }
    );
    drop(test);

    // Never complete: the stream ends in an error, not in a finished turn.
    let fixture = Fixture::serve(vec![Scripted::sse(frame(&events[..12], "\n"))]).await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let received_events = collect(
        test.provider
            .complete(&[Message::user("go")], &tools(), "system", None)
            .await
            .unwrap(),
    )
    .await;
    let last = received_events.last().expect("events");
    let error = format!("{:#}", last.as_ref().expect_err("the turn failed"));
    assert!(error.contains(STREAM_INCOMPLETE), "{error}");
    assert_eq!(fixture.requests().len(), MAX_RETRIES as usize);
}

#[tokio::test]
async fn a_consumer_that_stops_listening_is_a_cancellation_not_a_fault() {
    let fixture = Fixture::serve(vec![
        Scripted::sse(frame(&response_events(None), "\n")).in_pieces(3),
    ])
    .await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let request = test.provider.build_api_request(
        "claude-opus-5-5",
        &[Message::user("go")],
        &tools(),
        build_system_param("system", false),
        false,
    );
    let binding = jcode_provider_anthropic::binding::analyze_request(&request).binding;
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    stream_response(
        test.provider.client.clone(),
        &test.provider.route(false),
        "key".to_string(),
        request.clone(),
        SseStreamState::new(&request.model, binding),
        tx,
        "claude-opus-5-5",
    )
    .await
    .expect("a dropped receiver ends the attempt without an error");
}

/// user, then two tool rounds whose thinking is bound to the exact request
/// that produced it, on the API-key route.
fn bound_history(provider: &AnthropicProvider) -> Vec<Message> {
    let mut history = vec![Message::user("task")];
    for (signature, tool_id) in [("sig-1", "t1"), ("sig-2", "t2")] {
        let request = provider.build_api_request(
            &provider.model(),
            &history,
            &tools(),
            build_system_param("system", false),
            false,
        );
        let binding = jcode_provider_anthropic::binding::analyze_request(&request).binding;
        history.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::AnthropicThinking {
                    thinking: "t".to_string(),
                    signature: signature.to_string(),
                    binding: Some(AnthropicThinkingBinding {
                        model: provider.model(),
                        prefix_digest: binding.prefix_digest,
                        predecessor: binding.last_thinking,
                    }),
                },
                ContentBlock::ToolUse {
                    id: tool_id.to_string(),
                    name: "write".to_string(),
                    input: json!({"content": "x"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        });
        history.push(Message::tool_result(tool_id, "ok", false));
    }
    history
}

fn block_id(signature: &str) -> String {
    thinking_fingerprint(ThinkingPayload::Signature, signature)
}

#[tokio::test]
async fn provider_reported_drops_name_the_block_and_the_run_after_it() {
    for in_delta in [false, true] {
        let transformations = json!([
            {"type": "thinking_dropped", "reason": "prefix_binding_mismatch", "path": "messages.1.content.0"}
        ]);
        let mut events = response_events((!in_delta).then(|| transformations.clone()));
        if in_delta {
            let delta = events.len() - 2;
            events[delta]["input_transformations"] = transformations;
        }
        let fixture = Fixture::serve(vec![Scripted::sse(frame(&events, "\n"))]).await;
        let test = provider_for(&fixture, "claude-opus-5-5");
        let history = bound_history(&test.provider);
        let received_events = collect(
            test.provider
                .complete_split_with_context(&history, &tools(), "system", None, replanning())
                .await
                .unwrap(),
        )
        .await;
        let dropped: Vec<_> = received_events
            .iter()
            .flatten()
            .filter_map(|event| match event {
                StreamEvent::ProviderDroppedReasoning { block_ids, reason } => {
                    Some((block_ids.clone(), reason.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            dropped,
            vec![(
                vec![block_id("sig-1"), block_id("sig-2")],
                "prefix_binding_mismatch".to_string()
            )],
            "in message_delta: {in_delta}"
        );
        if !in_delta {
            // Nothing the API kept precedes the new block.
            let produced = received_events
                .iter()
                .flatten()
                .find_map(|event| match event {
                    StreamEvent::ReplayableReasoning(
                        ReplayableReasoningBlock::AnthropicThinking { binding, .. },
                    ) => Some(binding.clone()),
                    _ => None,
                })
                .expect("a produced block");
            assert_eq!(produced.predecessor, None);
        }
    }
}

#[tokio::test]
async fn unknown_drops_are_fed_back_and_routing_drops_are_kept() {
    let fixture = Fixture::serve(vec![
        Scripted::sse(frame(
            &response_events(Some(json!([
                {"type": "thinking_dropped", "reason": "model_binding_mismatch", "path": "messages.1.content.0"},
                {"type": "thinking_dropped", "reason": "organization_binding_mismatch", "path": "messages.3.content.0"}
            ]))),
            "\n",
        )),
        Scripted::sse(frame(
            &response_events(Some(json!([
                {"type": "some_future_drop", "reason": "some_future_reason", "path": "messages.3.content.0"}
            ]))),
            "\n",
        )),
    ])
    .await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let history = bound_history(&test.provider);
    let mut dropped = Vec::new();
    let mut predecessors = Vec::new();
    for _ in 0..2 {
        let events = collect(
            test.provider
                .complete_split_with_context(&history, &tools(), "system", None, replanning())
                .await
                .unwrap(),
        )
        .await;
        for event in events.into_iter().flatten() {
            match event {
                StreamEvent::ProviderDroppedReasoning { block_ids, reason } => {
                    dropped.push((block_ids, reason))
                }
                StreamEvent::ReplayableReasoning(ReplayableReasoningBlock::AnthropicThinking {
                    binding,
                    ..
                }) => predecessors.push(binding.predecessor),
                _ => {}
            }
        }
    }
    assert_eq!(
        dropped,
        vec![(vec![block_id("sig-2")], "some_future_reason".to_string())],
        "blocks another model or account cannot read stay in the transcript"
    );
    assert_eq!(
        predecessors,
        vec![Some(block_id("sig-2")), Some(block_id("sig-1"))],
        "a routing drop keeps the chain; a real drop chains to the last kept block"
    );
}

/// The recorded `prefix_mismatch_behavior: "error"` rejection (INT-01/WP-02
/// contract probe T3b, Opus 5.5, 2026-09-29), naming the second turn's block.
const RECORDED_BINDING_400: &str = "messages.3.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to \"drop_block\". Content before this block differs from when it was created, first at `messages.0.content.0`.";

#[tokio::test]
async fn a_rejection_of_replayed_thinking_asks_the_caller_to_replan_without_the_named_run() {
    for (message, expected, reason) in [
        (
            RECORDED_BINDING_400.to_string(),
            vec![block_id("sig-2")],
            "prefix_binding_mismatch",
        ),
        (
            RECORDED_BINDING_400.replace("messages.3.", "messages.1."),
            vec![block_id("sig-1"), block_id("sig-2")],
            "prefix_binding_mismatch",
        ),
        (
            // The documented modified-thinking rejection names no block: the
            // latest assistant message's thinking goes.
            "`thinking` or `redacted_thinking` blocks in the latest assistant message cannot be modified. These blocks must remain as they were in the original response.".to_string(),
            vec![block_id("sig-2")],
            "thinking_modified",
        ),
    ] {
        let fixture =
            Fixture::serve(vec![Scripted::error(400, "invalid_request_error", &message)]).await;
        let test = provider_for(&fixture, "claude-opus-5-5");
        let history = bound_history(&test.provider);
        let events = collect(
            test.provider
                .complete_split_with_context(&history, &tools(), "system", None, replanning())
                .await
                .unwrap(),
        )
        .await;
        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("the request was rejected");
        assert_eq!(
            ProviderRequestReplan::of(&error),
            Some(&ProviderRequestReplan::ReasoningRejected {
                block_ids: expected,
                reason: reason.to_string(),
            }),
            "{error:#}"
        );
        assert_eq!(fixture.requests().len(), 1, "the same request is not repeated");
    }
}

fn expected_body(provider: &AnthropicProvider, model: &str, history: &[Message]) -> Value {
    serde_json::to_value(provider.build_api_request(
        model,
        history,
        &tools(),
        build_system_param("system", false),
        false,
    ))
    .unwrap()
}

#[tokio::test]
async fn a_model_fallback_is_a_complete_new_plan() {
    let not_found = Scripted::error(
        404,
        "not_found_error",
        "model: claude-opus-5 is not available",
    );
    let ok = Scripted::sse(frame(&response_events(None), "\n"));

    // A caller that re-plans: the runtime stores the model and hands the
    // request back before anything else is sent.
    let fixture = Fixture::serve(vec![not_found.clone(), ok.clone()]).await;
    let test = provider_for(&fixture, "claude-opus-5");
    *test.provider.reasoning_effort.write().unwrap() = Some("none".to_string());
    let history = [Message::user("go")];
    let error = collect(
        test.provider
            .complete_split_with_context(&history, &tools(), "system", None, replanning())
            .await
            .unwrap(),
    )
    .await
    .into_iter()
    .find_map(Result::err)
    .expect("the unavailable model ends the request");
    let Some(ProviderRequestReplan::ModelFallback { from, to, .. }) =
        ProviderRequestReplan::of(&error)
    else {
        panic!("not a model fallback: {error:#}");
    };
    assert_eq!(from, "claude-opus-5");
    assert_eq!(test.provider.model(), *to);
    assert_ne!(to, "claude-opus-5");
    assert_eq!(fixture.requests().len(), 1);
    // The request the caller sends next is planned for the new model.
    let to = to.clone();
    let events = collect(
        test.provider
            .complete_split_with_context(&history, &tools(), "system", None, replanning())
            .await
            .unwrap(),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        fixture.requests()[1],
        expected_body(&test.provider, &to, &history)
    );
    drop(test);

    // A caller that does not re-plan: the retry carries every parameter of
    // the new model, not the old model's with a new id. Opus 5 at effort
    // `none` sends `thinking: disabled`, which its successors reject.
    let fixture = Fixture::serve(vec![not_found, ok]).await;
    let test = provider_for(&fixture, "claude-opus-5");
    *test.provider.reasoning_effort.write().unwrap() = Some("none".to_string());
    let events = collect(
        test.provider
            .complete(&history, &tools(), "system", None)
            .await
            .unwrap(),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["thinking"], json!({"type": "disabled"}));
    let fallback = test.provider.model();
    assert_eq!(
        requests[1],
        expected_body(&test.provider, &fallback, &history)
    );
    assert_ne!(
        requests[1]["thinking"],
        json!({"type": "disabled"}),
        "the old model's thinking configuration is not carried over"
    );
}

#[tokio::test]
async fn the_reasoning_self_heal_keeps_the_binding_control() {
    let fixture = Fixture::serve(vec![
        Scripted::error(
            400,
            "invalid_request_error",
            "This model does not support the effort parameter.",
        ),
        Scripted::sse(frame(&response_events(None), "\n")),
    ])
    .await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let events = collect(
        test.provider
            .complete(&[Message::user("go")], &tools(), "system", None)
            .await
            .unwrap(),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["output_config"], json!({"effort": "medium"}));
    assert!(requests[1].get("output_config").is_none());
    assert_eq!(
        requests[1]["thinking"],
        json!({
            "type": "adaptive",
            "display": "summarized",
            "block_binding": {"prefix_mismatch_behavior": "error"}
        })
    );
    assert!(requests[1].get("temperature").is_none());
}

#[tokio::test]
async fn a_request_whose_plan_resolved_differently_is_handed_back_not_sent() {
    let fixture = Fixture::serve(vec![Scripted::sse(frame(&response_events(None), "\n"))]).await;
    let test = provider_for(&fixture, "claude-opus-5-5");
    let mut history = bound_history(&test.provider);
    // The history no longer matches what the second turn was produced under.
    history[2] = Message::tool_result("t1", "ok (edited)", false);
    let error = test
        .provider
        .complete_split_with_context(&history, &tools(), "system", None, replanning())
        .await
        .err()
        .expect("a request that replays invalid thinking is not sent");
    assert_eq!(
        ProviderRequestReplan::of(&error),
        Some(&ProviderRequestReplan::ReplayedReasoningInvalid { blocks: 1 })
    );
    assert!(fixture.requests().is_empty());
    // After the caller suppresses what the runtime reports, it is sent.
    let invalid = test
        .provider
        .replayed_reasoning_invalidations(&history, &tools(), "system")
        .expect("a binding model");
    assert_eq!(invalid.len(), 1);
    history[invalid[0].message_index]
        .content
        .remove(invalid[0].block_index);
    let events = collect(
        test.provider
            .complete_split_with_context(&history, &tools(), "system", None, replanning())
            .await
            .unwrap(),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}

#[tokio::test]
async fn a_credential_mode_change_decides_the_next_request_route() {
    let lock = jcode_base::storage::lock_test_env();
    let home = tempfile::TempDir::new().unwrap();
    let _guards = [
        EnvGuard::set("JCODE_HOME", home.path()),
        EnvGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key"),
        EnvGuard::set("JCODE_RUNTIME_PROVIDER", "claude"),
    ];
    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "oauth-access".to_string(),
        refresh: String::new(),
        expires: i64::MAX,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();
    let provider = AnthropicProvider::new();
    *provider.model.write().unwrap() = "claude-opus-5-5".to_string();
    let api_key_history = bound_history(&provider);
    let invalid = |provider: &AnthropicProvider| {
        provider
            .replayed_reasoning_invalidations(&api_key_history, &tools(), "system")
            .expect("a binding model")
            .len()
    };

    // The previous request went out on OAuth: thinking bound to the API-key
    // prefix (no identity blocks) does not match it.
    provider
        .last_request_route
        .store(ROUTE_OAUTH, Ordering::Relaxed);
    assert_eq!(invalid(&provider), 2);
    // Choosing the API key decides the next request, whatever the last used.
    provider
        .set_credential_mode(AnthropicCredentialMode::ApiKey)
        .unwrap();
    assert_eq!(invalid(&provider), 0);
    // And back: the stale API-key observation does not outlive the choice.
    provider
        .last_request_route
        .store(ROUTE_API_KEY, Ordering::Relaxed);
    provider
        .set_credential_mode(AnthropicCredentialMode::OAuth)
        .unwrap();
    assert_eq!(invalid(&provider), 2);
    drop(lock);
}
