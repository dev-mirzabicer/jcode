/// Reports every bound Claude thinking block as invalid once the system
/// prompt is revised; the binding rules themselves are tested at the provider
/// and app-core layers. This checks the local request path's wiring.
#[derive(Clone)]
struct PrefixBoundTestProvider;

#[async_trait::async_trait]
impl Provider for PrefixBoundTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("PrefixBoundTestProvider")
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn reasoning_replay_kind(&self) -> Option<crate::provider::ContextReasoningBlockKind> {
        Some(crate::provider::ContextReasoningBlockKind::AnthropicThinking)
    }

    fn replayed_reasoning_invalidations(
        &self,
        messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        system: &str,
    ) -> Option<Vec<crate::provider::InvalidReplayedReasoning>> {
        if !system.contains("revised") {
            return Some(Vec::new());
        }
        Some(
            messages
                .iter()
                .enumerate()
                .flat_map(|(message_index, message)| {
                    message
                        .content
                        .iter()
                        .enumerate()
                        .filter(|(_, block)| {
                            matches!(
                                block,
                                crate::message::ContentBlock::AnthropicThinking {
                                    binding: Some(_),
                                    ..
                                }
                            )
                        })
                        .map(move |(block_index, _)| crate::provider::InvalidReplayedReasoning {
                            message_index,
                            block_index,
                            invalidity: crate::provider::ReplayedReasoningInvalidity::PrefixChanged,
                        })
                })
                .collect(),
        )
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn local_requests_suppress_stale_thinking_before_sending() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    let provider: Arc<dyn Provider> = Arc::new(PrefixBoundTestProvider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    // A fixed prompt keeps the prefix independent of shared instruction stores.
    app.ambient_system_prompt = Some("fixture system prompt".to_string());
    app.session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "task".to_string(),
            cache_control: None,
        }],
    );
    app.session.add_message(
        crate::message::Role::Assistant,
        vec![
            crate::message::ContentBlock::AnthropicThinking {
                thinking: "thought".to_string(),
                signature: "sig".to_string(),
                binding: Some(crate::message::AnthropicThinkingBinding {
                    model: "claude-sonnet-5-5".to_string(),
                    prefix_digest: "anthropic-prefix-v1:fixture".to_string(),
                    predecessor: None,
                }),
            },
            crate::message::ContentBlock::Text {
                text: "answer".to_string(),
                cache_control: None,
            },
        ],
    );

    assert_eq!(
        app.reconcile_local_replayed_reasoning("system", &[]),
        Ok(None),
        "an unchanged prefix changes nothing"
    );
    app.record_local_prefix_transition("skill activation", "fixture skill");
    let notice = app
        .reconcile_local_replayed_reasoning("system revised", &[])
        .expect("reconciled")
        .expect("the change is announced");
    assert!(notice.contains("1 Claude thinking block(s)"), "{notice}");
    assert!(app.pending_prefix_transitions.is_empty());
    let managed = app
        .session
        .context_view
        .active_reasoning_invalidation()
        .expect("persisted managed set");
    let jcode_session_types::StoredContextOperation::ReasoningSuppression(suppression) =
        &managed.operations[0]
    else {
        panic!("managed set holds suppressions");
    };
    assert_eq!(
        suppression.selection,
        jcode_session_types::StoredReasoningSelection::Invalidated {
            cause: jcode_session_types::StoredReasoningInvalidationCause::RequestPrefixChanged {
                recorded_transitions: vec!["skill activation".to_string()],
            },
        }
    );
    // Later context edits must stage against the local request prefix.
    assert!(app.local_request_prefix_if_needed().unwrap().is_some());
}

/// A registry tool that is never called; registering it changes membership.
struct LocalFixtureTool;

#[async_trait::async_trait]
impl crate::tool::Tool for LocalFixtureTool {
    fn name(&self) -> &str {
        "local_fixture_extra"
    }
    fn description(&self) -> &str {
        "fixture"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn decode_input(&self, _input: &serde_json::Value) -> Result<()> {
        Ok(())
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tool::ToolContext,
    ) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new("ok"))
    }
}

#[test]
fn local_requests_keep_the_frozen_tool_set_and_announce_a_change_once() {
    // INT-01/WP-06 (D15): the local loop follows the agent's tool-set rule.
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    let provider: Arc<dyn Provider> = Arc::new(PrefixBoundTestProvider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.ambient_system_prompt = Some("fixture system prompt".to_string());
    // Frozen tool guidance keeps the set independent of shared instruction
    // stores, as a session that already sent a request has it.
    app.session.delegation_guidance = Some("fixture delegation guidance".to_string());
    let notices = |app: &App| crate::tool::tool_set_notice_count(&app.session.messages);

    // The first request freezes the set and persists it with the session.
    let first = rt.block_on(app.local_tool_definitions()).expect("tools");
    let again = rt.block_on(app.local_tool_definitions()).expect("tools");
    assert_eq!(first, again);
    assert_eq!(
        app.session.tool_set.as_ref().map(|set| &set.advertised),
        Some(&first)
    );
    assert_eq!(notices(&app), 0);
    assert!(app.pending_prefix_transitions.is_empty());

    // A registry change is announced once, as a delivery appended to the
    // session the request is projected from. This provider's array must
    // carry the addition, which is a recorded tool-set transition.
    rt.block_on(app.registry.register(
        "local_fixture_extra".to_string(),
        Arc::new(LocalFixtureTool),
    ));
    let changed = rt.block_on(app.local_tool_definitions()).expect("tools");
    assert_eq!(changed[..first.len()], first[..]);
    assert_eq!(
        changed.last().map(|tool| tool.name.as_str()),
        Some("local_fixture_extra")
    );
    assert_eq!(notices(&app), 1);
    assert!(matches!(
        app.session
            .messages
            .last()
            .and_then(|message| message.operator_delivery())
            .map(|delivery| delivery.tool_changes),
        Some([crate::message::ToolSetChange::Added { definition }])
            if definition.name == "local_fixture_extra"
    ));
    assert_eq!(
        app.pending_prefix_transitions,
        vec![crate::tool::TOOL_SET_TRANSITION.to_string()]
    );

    let settled = rt.block_on(app.local_tool_definitions()).expect("tools");
    assert_eq!(settled, changed);
    assert_eq!(notices(&app), 1);
}
