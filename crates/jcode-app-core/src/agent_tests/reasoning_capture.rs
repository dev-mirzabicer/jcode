// INT-01/WP-02: both agent turn loops store a provider's reasoning through the
// shared assembler, in stream order, and discard a rolled-back attempt.

/// Streams an Anthropic-shaped response: a signed thinking block, text, an
/// empty-text signed block (a progress update under `display: "omitted"`)
/// and more text. The first attempt is rolled back mid-stream.
#[derive(Clone)]
struct ReasoningStreamProvider {
    replay_kind: Option<jcode_provider_core::ContextReasoningBlockKind>,
}

fn capture_binding() -> crate::message::AnthropicThinkingBinding {
    crate::message::AnthropicThinkingBinding {
        model: "claude-opus-5-5".to_string(),
        prefix_digest: "anthropic-prefix-v1:capture".to_string(),
        predecessor: None,
    }
}

fn signed(text: &str, signature: &str) -> StreamEvent {
    StreamEvent::ReplayableReasoning(crate::message::ReplayableReasoningBlock::AnthropicThinking {
        thinking: text.to_string(),
        signature: signature.to_string(),
        binding: capture_binding(),
    })
}

#[async_trait]
impl Provider for ReasoningStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel(32);
        tokio::spawn(async move {
            let events = vec![
                // Attempt 1, rolled back by a transport retry.
                StreamEvent::ThinkingStart,
                StreamEvent::ThinkingDelta("stale".into()),
                signed("stale", "sig-stale"),
                StreamEvent::ThinkingEnd,
                StreamEvent::TextDelta("stale text".into()),
                StreamEvent::RetryRollback { attempt: 2, max: 3 },
                // Attempt 2.
                StreamEvent::ThinkingStart,
                StreamEvent::ThinkingDelta("plan".into()),
                signed("plan", "sig-a"),
                StreamEvent::ThinkingEnd,
                StreamEvent::TextDelta("Looking.".into()),
                StreamEvent::ThinkingStart,
                signed("", "sig-b"),
                StreamEvent::ThinkingEnd,
                StreamEvent::TextDelta(" Done.".into()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ];
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }
    fn name(&self) -> &str {
        "reasoning-capture-fixture"
    }
    fn reasoning_replay_kind(&self) -> Option<jcode_provider_core::ContextReasoningBlockKind> {
        self.replay_kind
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn stored_turn_shape(agent: &Agent) -> Vec<String> {
    let assistant = agent
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == crate::message::Role::Assistant)
        .expect("an assistant turn was stored");
    assistant
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::AnthropicThinking {
                thinking,
                signature,
                binding,
            } => {
                assert_eq!(binding.as_ref(), Some(&capture_binding()));
                format!("thinking:{signature}:{thinking}")
            }
            ContentBlock::ReasoningTrace { text } => format!("trace:{text}"),
            ContentBlock::Text { text, .. } => format!("text:{text}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn both_agent_loops_store_reasoning_in_stream_order_through_the_assembler() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let replaying = Some(jcode_provider_core::ContextReasoningBlockKind::AnthropicThinking);
    for (replay_kind, expected) in [
        (
            replaying,
            vec![
                "thinking:sig-a:plan",
                "text:Looking.",
                "thinking:sig-b:",
                "text: Done.",
            ],
        ),
        (None, vec!["trace:plan", "text:Looking. Done."]),
    ] {
        for streaming in [false, true] {
            let provider: Arc<dyn Provider> = Arc::new(ReasoningStreamProvider { replay_kind });
            let registry = Registry::new(provider.clone()).await;
            let mut agent = Agent::new(provider, registry);
            if streaming {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                agent
                    .run_once_streaming_mpsc("synthetic task", Vec::new(), None, tx)
                    .await?;
            } else {
                agent.run_once_capture("synthetic task").await?;
            }
            assert_eq!(
                stored_turn_shape(&agent),
                expected,
                "streaming={streaming} replay={replay_kind:?}"
            );
        }
    }
    Ok(())
}
