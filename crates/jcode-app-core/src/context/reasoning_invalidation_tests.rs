//! Context transitions and request-time checks against replayed reasoning
//! bound to its request prefix (INT-01 WP-04, R11).
//!
//! The fixture provider decides validity with the production Anthropic
//! formatter and `binding::blocks_to_suppress`, and every produced turn is
//! bound to the exact projected request it came from, so these tests exercise
//! the real prefix rules rather than a model of them.

use super::commit::{
    ContextPersistence, DirectContextSessionPersistence, LocalContextRoute,
    prepare_context_transition_for_session,
};
use super::draft::{ContextServiceLimits, ContextTransactionService, state_with_transaction};
use super::reasoning_invalidation::{
    ContextRequestPrefix, reconcile_before_request, stage_for_transition,
};
use crate::agent::Agent;
use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::protocol::ContextServiceError;
use crate::provider::{
    ContextProjectionValidationOperation, ContextProjectionValidationReport, ContextProviderFamily,
    ContextProviderValidationIdentity, ContextReasoningBlockKind, EventStream,
    InvalidReplayedReasoning, Provider, ReplayedReasoningInvalidity,
    context_projection_validation_report,
};
use crate::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use jcode_context_core::{
    build_content_target, project_context, resolve_reasoning_suppression_for_ranges,
    resolve_reasoning_suppression_keep_latest,
};
use jcode_message_types::AnthropicThinkingBinding;
use jcode_provider_anthropic::binding::{analyze_request, stored_thinking_fingerprint};
use jcode_provider_anthropic::{
    ApiRequest, ApiToolChoice, build_system_param, format_messages, format_tools,
};
use jcode_session_types::{
    StoredContextAuthorization, StoredContextOperation, StoredContextTransactionStatusKind,
    StoredContextTransitionKind, StoredContextViewState, StoredReasoningInvalidationCause,
    StoredReasoningSelection, StoredToolResultDistillation,
};
use std::collections::HashMap;
use std::sync::Arc;

const SYSTEM: &str = "fixture system prompt";

/// A Claude-like runtime: prefix-bound unless `binds` is false (an Unbound
/// model such as Opus 5, INT-01 D11).
#[derive(Clone)]
struct BindingProvider {
    binds: bool,
}

pub(crate) fn fixture_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
) -> ApiRequest {
    let api_tools = format_tools(tools);
    let mut request = ApiRequest {
        model: "claude-sonnet-5-5".to_string(),
        max_tokens: 1024,
        system: build_system_param(system, false),
        messages: format_messages(messages),
        tool_choice: ApiToolChoice::for_tools(&api_tools),
        tools: (!api_tools.is_empty()).then_some(api_tools),
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    };
    // As `build_api_request` does; bindings exclude every marker.
    jcode_provider_anthropic::place_cache_breakpoints(&mut request, false);
    request
}

#[async_trait]
impl Provider for BindingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty::<Result<StreamEvent>>()))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> String {
        if self.binds {
            "claude-sonnet-5-5".to_string()
        } else {
            "claude-opus-5".to_string()
        }
    }

    fn context_window(&self) -> usize {
        1_000_000
    }

    fn reasoning_replay_kind(&self) -> Option<ContextReasoningBlockKind> {
        Some(ContextReasoningBlockKind::AnthropicThinking)
    }

    fn validate_projected_context(
        &self,
        messages: &[Message],
        operations: &[ContextProjectionValidationOperation],
    ) -> ContextProjectionValidationReport {
        context_projection_validation_report(
            ContextProviderValidationIdentity {
                family: ContextProviderFamily::Anthropic,
                provider_name: self.name().to_string(),
                provider_display_name: "Anthropic fixture".to_string(),
                model: self.model(),
                evidence_tag: "anthropic_fixture".to_string(),
            },
            operations,
            Some(ContextReasoningBlockKind::AnthropicThinking),
            jcode_provider_anthropic::validate_projected_messages(messages),
        )
    }

    fn replayed_reasoning_invalidations(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> Option<Vec<InvalidReplayedReasoning>> {
        self.binds
            .then(|| fixture_invalidations(messages, tools, system))
    }

    fn replayed_reasoning_block_id(&self, block: &ContentBlock) -> Option<String> {
        stored_thinking_fingerprint(block)
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// The production planner applied to the fixture request: the blocks a
/// prefix-bound runtime reports, as indices into `messages`.
pub(crate) fn fixture_invalidations(
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
) -> Vec<InvalidReplayedReasoning> {
    jcode_provider_anthropic::binding::plan_suppressions(messages, |candidate| {
        fixture_request(candidate, tools, system)
    })
    .into_iter()
    .map(|block| InvalidReplayedReasoning {
        message_index: block.message_index,
        block_index: block.block_index,
        invalidity: if block.validity
            == jcode_provider_anthropic::binding::ReplayValidity::ChainBroken
        {
            ReplayedReasoningInvalidity::ChainBroken
        } else {
            ReplayedReasoningInvalidity::PrefixChanged
        },
    })
    .collect()
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => crate::env::set_var(self.key, previous),
            None => crate::env::remove_var(self.key),
        }
    }
}

struct NoPersistence;

impl ContextPersistence for NoPersistence {
    fn persist(&self, _agent: &mut Agent) -> Result<()> {
        Ok(())
    }
}

impl DirectContextSessionPersistence for NoPersistence {
    fn persist(&self, _session: &mut Session) -> Result<()> {
        Ok(())
    }
}

fn tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "bash".to_string(),
        description: "Run a command".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }),
    }]
}

fn prefix() -> ContextRequestPrefix {
    ContextRequestPrefix {
        system: SYSTEM.to_string(),
        tools: tools(),
        recorded_transitions: Vec::new(),
    }
}

/// A Claude session whose turns are bound to the projected request that
/// produced them, as the runtime binds them.
struct Conversation {
    session: Session,
    provider: BindingProvider,
    /// Stored message index of each turn's thinking block, by signature.
    turns: HashMap<String, usize>,
    service: ContextTransactionService,
}

impl Conversation {
    fn new(binds: bool) -> Self {
        let mut session = Session::create(None, None);
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "task".to_string(),
                cache_control: None,
            }],
        );
        Self {
            session,
            provider: BindingProvider { binds },
            turns: HashMap::new(),
            service: ContextTransactionService::with_persistence_boundaries(
                ContextServiceLimits::default(),
                Arc::new(NoPersistence),
                Arc::new(NoPersistence),
            ),
        }
    }

    /// The messages a request sends now, stamped as the request loops
    /// stamp them.
    fn projected(&self) -> Vec<Message> {
        super::reasoning_invalidation::request_messages(
            &self.provider,
            &self.session.messages,
            project_context(&self.session.messages, &self.session.context_view)
                .expect("projection")
                .messages,
        )
    }

    /// One tool round: a signed thinking block and a tool call bound to the
    /// current projected request, then its result.
    fn round(&mut self, signature: &str) {
        let report = analyze_request(&fixture_request(&self.projected(), &tools(), SYSTEM));
        let tool_id = format!("tool-{signature}");
        let index = self.session.messages.len();
        self.session.add_message(
            Role::Assistant,
            vec![
                ContentBlock::AnthropicThinking {
                    thinking: format!("thought {signature}"),
                    signature: signature.to_string(),
                    binding: Some(AnthropicThinkingBinding {
                        model: "claude-sonnet-5-5".to_string(),
                        prefix_digest: report.binding.prefix_digest,
                        predecessor: report.binding.last_thinking,
                    }),
                },
                ContentBlock::ToolUse {
                    id: tool_id.clone(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "true"}),
                    thought_signature: None,
                },
            ],
        );
        self.session.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: tool_id,
                content: format!("result of {signature} ").repeat(40),
                is_error: None,
            }],
        );
        self.turns.insert(signature.to_string(), index);
    }

    fn index(&self, signature: &str) -> usize {
        self.turns[signature]
    }

    /// Apply a person's transaction through the production transition path.
    fn apply(&mut self, id: &str, operations: Vec<StoredContextOperation>) -> ApplyOutcome {
        let previous = self.session.context_view.clone();
        let proposed = state_with_transaction(
            &previous,
            id,
            previous.revision + 1,
            StoredContextAuthorization::Manual { initiated_by: None },
            operations,
            None,
            Vec::new(),
        );
        let index = proposed.transactions.len() - 1;
        let prepared = prepare_context_transition_for_session(
            &self.provider,
            &self.session.messages,
            &previous,
            proposed,
            index,
            true,
            "fixture-route",
            None,
            &prefix(),
        )
        .expect("prepared transition");
        self.session.context_view = prepared.state;
        ApplyOutcome {
            summary: prepared.result.reasoning_invalidation,
            deleted_input_tokens: prepared
                .result
                .transaction
                .economics
                .map(|economics| economics.deleted_input_tokens)
                .unwrap_or_default(),
        }
    }

    fn revert(&mut self, id: &str) -> Option<crate::protocol::ContextReasoningInvalidationSummary> {
        let provider = self.provider.clone();
        let prefix = prefix();
        self.service
            .revert_transaction_in_session(
                &mut self.session,
                LocalContextRoute {
                    provider: &provider,
                    route: "fixture-route",
                    estimated_total_request_tokens_before: None,
                    prefix: &prefix,
                },
                id,
                false,
            )
            .expect("revert")
            .result
            .reasoning_invalidation
    }

    fn reapply(
        &mut self,
        id: &str,
    ) -> Option<crate::protocol::ContextReasoningInvalidationSummary> {
        let provider = self.provider.clone();
        let prefix = prefix();
        self.service
            .reapply_transaction_in_session(
                &mut self.session,
                LocalContextRoute {
                    provider: &provider,
                    route: "fixture-route",
                    estimated_total_request_tokens_before: None,
                    prefix: &prefix,
                },
                id,
                false,
            )
            .expect("reapply")
            .result
            .reasoning_invalidation
    }

    /// Signatures the managed set holds, with their causes.
    fn suppressed(&self) -> Vec<(String, StoredReasoningInvalidationCause)> {
        let mut held = Vec::new();
        let Some(transaction) = self.session.context_view.active_reasoning_invalidation() else {
            return held;
        };
        for operation in &transaction.operations {
            let StoredContextOperation::ReasoningSuppression(suppression) = operation else {
                panic!("managed transactions hold only suppressions");
            };
            let StoredReasoningSelection::Invalidated { cause } = &suppression.selection else {
                panic!("managed suppressions carry a cause");
            };
            for target in &suppression.targets {
                held.push((self.signature_at(target.stored_index_hint), cause.clone()));
            }
        }
        held.sort();
        held
    }

    fn suppressed_signatures(&self) -> Vec<String> {
        self.suppressed()
            .into_iter()
            .map(|(signature, _)| signature)
            .collect()
    }

    fn signature_at(&self, index: usize) -> String {
        match &self.session.messages[index].content[0] {
            ContentBlock::AnthropicThinking { signature, .. } => signature.clone(),
            other => panic!("message {index} starts with {other:?}"),
        }
    }

    /// What a request sends now: every replayed block must be valid.
    fn assert_every_replayed_block_is_valid(&self) {
        let request = fixture_request(&self.projected(), &tools(), SYSTEM);
        let report = analyze_request(&request);
        assert!(
            report.invalid().next().is_none(),
            "invalid blocks would be sent: {:?}",
            report.replayed
        );
    }

    fn replayed_signatures(&self) -> Vec<String> {
        let mut signatures: Vec<String> = self
            .projected()
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::AnthropicThinking { signature, .. } => Some(signature.clone()),
                _ => None,
            })
            .collect();
        signatures.sort();
        signatures
    }

    fn summary_of(&self, first: &str, last_result_of: &str) -> StoredContextOperation {
        // From the assistant message of `first` through the tool result that
        // follows `last_result_of`.
        let start = self.index(first);
        let end = self.index(last_result_of) + 1;
        let range = jcode_context_core::build_message_range(&self.session.messages, start, end)
            .expect("range");
        StoredContextOperation::RangeSummary(jcode_session_types::StoredRangeSummary {
            source_range: range,
            summary_text: format!("Summary of {first} through {last_result_of}."),
            file_change_digest: String::new(),
            changed_files: Vec::new(),
            change_evidence_complete: true,
            file_evidence: None,
            boundary_expansions: Vec::new(),
            generator: None,
            source_token_estimate: 0,
            replacement_token_estimate: 0,
            warnings: Vec::new(),
            created_at: Utc::now(),
            legacy_coverage: None,
        })
    }

    fn distillation_of(&self, signature: &str) -> StoredContextOperation {
        let index = self.index(signature) + 1;
        let target = build_content_target(&self.session.messages, index, 0).expect("target");
        let original = jcode_context_core::estimate_content_block_tokens(
            &self.session.messages[index].content[0],
        );
        let replacement_token_estimate = 1;
        StoredContextOperation::ToolResultDistillation(StoredToolResultDistillation {
            target,
            tool_name: "bash".to_string(),
            tool_call_id: format!("tool-{signature}"),
            replacement_content: "ok".to_string(),
            original_token_estimate: original,
            replacement_token_estimate,
            replacement_ratio_millionths: ((replacement_token_estimate as u128 * 1_000_000)
                / original as u128) as u32,
            preservation_rationale: "fixture".to_string(),
            uncertainties: Vec::new(),
            generator: jcode_session_types::StoredContextArtifactGenerator {
                provider: "fixture".to_string(),
                model: "fixture".to_string(),
                route: "fixture".to_string(),
                prompt_version: "fixture".to_string(),
                effort: None,
                role: None,
                selection_source: None,
                transaction_instructions: None,
                task_instructions: None,
            },
            created_at: Utc::now(),
        })
    }
}

struct ApplyOutcome {
    summary: Option<crate::protocol::ContextReasoningInvalidationSummary>,
    deleted_input_tokens: usize,
}

fn transition(id: &str, kind: StoredContextTransitionKind) -> StoredReasoningInvalidationCause {
    StoredReasoningInvalidationCause::ContextTransition {
        transaction_id: id.to_string(),
        transition: kind,
    }
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn a_summary_suppresses_the_later_thinking_it_invalidates_and_keeps_the_earlier() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c", "d"] {
        chat.round(signature);
    }
    let before = chat.projected();
    let summary = chat.summary_of("b", "b");
    let outcome = chat.apply("summary", vec![summary]);

    // `a` precedes the summarized range; `b` is inside it (covered); `c` and
    // `d` follow it and are bound to the unsummarized history.
    assert_eq!(chat.suppressed_signatures(), names(&["c", "d"]));
    assert!(
        chat.suppressed()
            .iter()
            .all(|(_, cause)| *cause == transition("summary", StoredContextTransitionKind::Apply))
    );
    assert_eq!(chat.replayed_signatures(), names(&["a"]));
    chat.assert_every_replayed_block_is_valid();

    let summary = outcome.summary.expect("the change is reported");
    assert_eq!(summary.invalidated_by_change, 2);
    assert_eq!(summary.invalidated_other, 0);
    assert_eq!(summary.restored, 0);
    assert_eq!(summary.active, 2);
    assert!(summary.invalidated_by_change_tokens > 0);

    // One revision holds the person's transaction and the managed set, and
    // the transaction's economics include the removed reasoning.
    let state = &chat.session.context_view;
    assert_eq!(state.revision, 1);
    let managed = state.active_reasoning_invalidation().expect("managed set");
    assert_eq!(managed.status_events[0].revision, 1);
    assert_eq!(managed.id, summary.transaction_id.clone().unwrap());
    assert!(!before.is_empty());
    let summary_only = {
        let mut probe = Conversation::new(false);
        probe.session = chat.session.clone();
        probe.turns = chat.turns.clone();
        probe.session.context_view = StoredContextViewState::default();
        let summary = probe.summary_of("b", "b");
        probe.apply("summary", vec![summary]).deleted_input_tokens
    };
    assert!(
        outcome.deleted_input_tokens > summary_only,
        "economics include the suppressed reasoning ({} vs {summary_only})",
        outcome.deleted_input_tokens
    );
}

#[test]
fn a_distillation_suppresses_the_thinking_after_the_changed_result() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let distillation = chat.distillation_of("a");
    chat.apply("distill", vec![distillation]);
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c"]));
    assert_eq!(chat.replayed_signatures(), names(&["a"]));
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn a_suppressed_middle_block_suppresses_the_blocks_chained_after_it() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let middle = jcode_context_core::build_message_range(
        &chat.session.messages,
        chat.index("b"),
        chat.index("b"),
    )
    .unwrap();
    let suppression =
        resolve_reasoning_suppression_for_ranges(&chat.session.messages, &[middle]).unwrap();
    let outcome = chat.apply(
        "middle",
        vec![StoredContextOperation::ReasoningSuppression(suppression)],
    );
    assert_eq!(chat.suppressed_signatures(), names(&["c"]));
    assert_eq!(chat.replayed_signatures(), names(&["a"]));
    assert_eq!(outcome.summary.unwrap().invalidated_by_change, 1);
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn keeping_the_latest_turns_removes_a_leading_run_and_stages_nothing() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let suppression = resolve_reasoning_suppression_keep_latest(&chat.session.messages, 1).unwrap();
    let outcome = chat.apply(
        "keep-latest",
        vec![StoredContextOperation::ReasoningSuppression(suppression)],
    );
    assert!(
        chat.session
            .context_view
            .active_reasoning_invalidation()
            .is_none()
    );
    assert_eq!(chat.replayed_signatures(), names(&["c"]));
    let summary = outcome.summary;
    assert!(summary.is_none(), "{summary:?}");
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn revert_restores_what_matches_again_and_suppresses_what_the_summary_produced() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    chat.apply("summary", vec![summary]);
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c"]));

    // Produced while the summary is active: bound to the summarized history.
    chat.round("d");
    chat.assert_every_replayed_block_is_valid();

    let reverted = chat.revert("summary").expect("revert changes reasoning");
    assert_eq!(chat.suppressed_signatures(), names(&["d"]));
    assert_eq!(
        chat.suppressed(),
        vec![(
            "d".to_string(),
            transition("summary", StoredContextTransitionKind::Revert)
        )]
    );
    assert_eq!(reverted.restored, 2, "b and c match the raw history again");
    assert_eq!(reverted.invalidated_by_change, 1);
    assert_eq!(chat.replayed_signatures(), names(&["a", "b", "c"]));
    chat.assert_every_replayed_block_is_valid();

    // Produced after the revert: bound to the raw history.
    chat.round("e");
    let reapplied = chat.reapply("summary").expect("reapply changes reasoning");
    // `d` matches the summarized history again; `b`, `c` and `e` do not.
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c", "e"]));
    assert_eq!(reapplied.restored, 1);
    assert_eq!(reapplied.invalidated_by_change, 3);
    assert_eq!(chat.replayed_signatures(), names(&["d"]));
    chat.assert_every_replayed_block_is_valid();
    assert!(chat.suppressed().iter().all(|(_, cause)| *cause
        == transition("summary", StoredContextTransitionKind::Reapply)));

    // Every managed set is kept as history; only the newest is active.
    let managed: Vec<_> = chat
        .session
        .context_view
        .transactions
        .iter()
        .filter(|transaction| transaction.is_reasoning_invalidation())
        .collect();
    assert_eq!(managed.len(), 3);
    assert_eq!(
        managed
            .iter()
            .filter(|transaction| transaction.is_active())
            .count(),
        1
    );
    assert!(managed[..2].iter().all(|transaction| {
        transaction
            .latest_status()
            .is_some_and(|status| status.kind == StoredContextTransactionStatusKind::Superseded)
    }));
}

#[test]
fn overlapping_transactions_stay_exact_in_every_order() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("b", "b");
    chat.apply("t1", vec![summary]);
    chat.round("d"); // bound to the history with t1 active
    let distillation = chat.distillation_of("a");
    chat.apply("t2", vec![distillation]);
    // Everything after a's result is now invalid; a stays valid.
    assert_eq!(chat.suppressed_signatures(), names(&["c", "d"]));
    chat.assert_every_replayed_block_is_valid();

    chat.revert("t1");
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c", "d"]));
    chat.assert_every_replayed_block_is_valid();

    chat.revert("t2");
    // Raw history again: b and c match; d was produced under t1.
    assert_eq!(chat.suppressed_signatures(), names(&["d"]));
    chat.assert_every_replayed_block_is_valid();

    chat.reapply("t1");
    // Exactly the state before t2: c invalid, d valid again.
    assert_eq!(chat.suppressed_signatures(), names(&["c"]));
    assert_eq!(chat.replayed_signatures(), names(&["a", "d"]));
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn an_unbound_model_stages_and_reports_nothing() {
    let mut chat = Conversation::new(false);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    let outcome = chat.apply("summary", vec![summary]);
    assert!(outcome.summary.is_none());
    assert!(
        chat.session
            .context_view
            .transactions
            .iter()
            .all(|transaction| !transaction.is_reasoning_invalidation())
    );
    assert_eq!(chat.replayed_signatures(), names(&["b", "c"]));
    assert!(chat.revert("summary").is_none());
}

/// A route or model that does not bind reasoning changes nothing in the
/// managed set (D16, INT-01/WP-06 R21): no lift, which would send the
/// suppressed blocks again and lose the reasoning chained after them on the
/// way back, and no additions. Back on the binding model the exact
/// recompute finds the same set.
#[test]
fn switching_to_an_unbound_model_and_back_restores_and_strips_nothing() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    chat.apply("summary", vec![summary]);
    assert_eq!(chat.suppressed_signatures(), names(&["b"]));
    let held = chat.session.context_view.clone();

    let unbound = BindingProvider { binds: false };
    let reconcile = |chat: &Conversation, provider: &BindingProvider| {
        reconcile_before_request(
            provider,
            &chat.session.messages,
            &chat.session.context_view,
            &prefix(),
            &[],
        )
        .unwrap()
    };
    assert!(
        reconcile(&chat, &unbound).is_none(),
        "the unbound route changes nothing"
    );
    // Two turns on the unbound model, produced with the set in force.
    chat.round("x");
    assert!(reconcile(&chat, &unbound).is_none());
    chat.round("y");
    assert_eq!(chat.session.context_view.transactions, held.transactions);

    // Back on the binding model: nothing restored, nothing stripped, and
    // every block the request sends is valid.
    assert!(reconcile(&chat, &chat.provider).is_none());
    assert_eq!(chat.suppressed_signatures(), names(&["b"]));
    // `a` is inside the summarized range.
    assert_eq!(chat.replayed_signatures(), names(&["x", "y"]));
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn a_changed_prefix_is_suppressed_before_the_request_and_restored_when_it_returns() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b"] {
        chat.round(signature);
    }
    assert!(
        reconcile_before_request(
            &chat.provider,
            &chat.session.messages,
            &chat.session.context_view,
            &prefix(),
            &[]
        )
        .unwrap()
        .is_none(),
        "an unchanged prefix changes nothing"
    );

    let mut skill = prefix();
    skill.system.push_str("\n\n# Active Skill\nfixture");
    skill.recorded_transitions = vec!["skill activation".to_string()];
    let outcome = reconcile_before_request(
        &chat.provider,
        &chat.session.messages,
        &chat.session.context_view,
        &skill,
        &[],
    )
    .unwrap()
    .expect("every earlier block stops matching");
    chat.session.context_view = outcome.state;
    assert_eq!(chat.session.context_view.revision, 1);
    assert_eq!(
        chat.suppressed(),
        vec![
            (
                "a".to_string(),
                StoredReasoningInvalidationCause::RequestPrefixChanged {
                    recorded_transitions: names(&["skill activation"]),
                }
            ),
            (
                "b".to_string(),
                StoredReasoningInvalidationCause::RequestPrefixChanged {
                    recorded_transitions: names(&["skill activation"]),
                }
            ),
        ]
    );
    let summary = outcome.summary.unwrap();
    assert_eq!(summary.invalidated_other, 2);
    assert!(
        super::describe_reasoning_invalidation(&summary)
            .unwrap()
            .contains("2 Claude thinking block(s)")
    );

    let mut tools_changed = prefix();
    tools_changed.tools.push(ToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    });
    assert!(
        reconcile_before_request(
            &chat.provider,
            &chat.session.messages,
            &chat.session.context_view,
            &tools_changed,
            &[]
        )
        .unwrap()
        .is_none(),
        "the set is already complete; the cause is kept"
    );

    let outcome = reconcile_before_request(
        &chat.provider,
        &chat.session.messages,
        &chat.session.context_view,
        &prefix(),
        &[],
    )
    .unwrap()
    .expect("the original prefix makes them valid again");
    chat.session.context_view = outcome.state;
    assert!(
        chat.session
            .context_view
            .active_reasoning_invalidation()
            .is_none()
    );
    assert_eq!(outcome.summary.unwrap().restored, 2);
    chat.assert_every_replayed_block_is_valid();
}

/// What the provider reports as dropped or rejected joins the managed set
/// with the provider's reason as its cause (INT-01/WP-06 R19).
#[test]
fn provider_reported_blocks_join_the_managed_set_with_their_cause() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let id = |signature: &str| {
        jcode_provider_anthropic::binding::thinking_fingerprint(
            jcode_provider_anthropic::binding::ThinkingPayload::Signature,
            signature,
        )
    };
    // jcode's own check finds every block valid; the provider dropped `b`
    // and, as it always does, the run after it.
    let reported = [super::ProviderReportedReasoning {
        block_ids: vec![
            id("b"),
            id("c"),
            "an-identity-no-stored-block-has".to_string(),
        ],
        reason: "prefix_binding_mismatch".to_string(),
    }];
    let outcome = reconcile_before_request(
        &chat.provider,
        &chat.session.messages,
        &chat.session.context_view,
        &prefix(),
        &reported,
    )
    .unwrap()
    .expect("the reported blocks are suppressed");
    chat.session.context_view = outcome.state;
    let cause = StoredReasoningInvalidationCause::ProviderReported {
        reason: "prefix_binding_mismatch".to_string(),
    };
    assert_eq!(
        chat.suppressed(),
        vec![
            ("b".to_string(), cause.clone()),
            ("c".to_string(), cause.clone())
        ]
    );
    assert_eq!(outcome.summary.unwrap().invalidated_other, 2);
    assert_eq!(chat.replayed_signatures(), names(&["a"]));
    chat.assert_every_replayed_block_is_valid();

    // Reported again, or not at all: nothing changes, and the next request
    // no longer sends them.
    for again in [&reported[..], &[]] {
        assert!(
            reconcile_before_request(
                &chat.provider,
                &chat.session.messages,
                &chat.session.context_view,
                &prefix(),
                again,
            )
            .unwrap()
            .is_none()
        );
    }

    // A report is persisted on a route that does not bind reasoning too.
    let mut other = Conversation::new(false);
    for signature in ["a", "b"] {
        other.round(signature);
    }
    let outcome = reconcile_before_request(
        &other.provider,
        &other.session.messages,
        &other.session.context_view,
        &prefix(),
        &[super::ProviderReportedReasoning {
            block_ids: vec![id("b")],
            reason: "some_future_reason".to_string(),
        }],
    )
    .unwrap()
    .expect("the reported block is suppressed");
    other.session.context_view = outcome.state;
    assert_eq!(other.suppressed_signatures(), names(&["b"]));
}

#[test]
fn a_summary_over_thinking_jcode_suppressed_does_not_shadow_the_managed_set() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    // A skill activation changes the prefix; the next request suppresses every
    // earlier block in the managed set.
    let mut skill = prefix();
    skill.system.push_str("\n\n# Active Skill\nfixture");
    skill.recorded_transitions = vec!["skill activation".to_string()];
    let outcome = reconcile_before_request(
        &chat.provider,
        &chat.session.messages,
        &chat.session.context_view,
        &skill,
        &[],
    )
    .unwrap()
    .expect("every earlier block stops matching");
    chat.session.context_view = outcome.state;
    assert_eq!(chat.suppressed_signatures(), names(&["a", "b", "c"]));

    let range_over = |chat: &Conversation, signature: &str| {
        let id = |index: usize| chat.session.messages[index].id.clone();
        chat.service.preview_context_ranges_for_session(
            &chat.session,
            chat.session.context_view.revision,
            jcode_context_core::authoritative_transcript_digest(&chat.session.messages),
            &[crate::protocol::ContextMessageRangeSelection {
                start_message_id: id(chat.index(signature)),
                end_message_id: id(chat.index(signature) + 1),
            }],
        )
    };
    let preview = range_over(&chat, "a").expect("a summary may cover managed suppressions");
    assert!(
        preview.shadowed_active_operations.is_empty(),
        "the managed set is recomputed, not shadowed: {:?}",
        preview.shadowed_active_operations
    );

    // A person's own suppression is still an operation a summary shadows.
    let b = jcode_context_core::build_message_range(
        &chat.session.messages,
        chat.index("b"),
        chat.index("b"),
    )
    .unwrap();
    let suppression =
        resolve_reasoning_suppression_for_ranges(&chat.session.messages, &[b]).unwrap();
    let mut changed = skill.clone();
    changed.recorded_transitions.clear();
    let previous = chat.session.context_view.clone();
    let proposed = state_with_transaction(
        &previous,
        "explicit-b",
        previous.revision + 1,
        StoredContextAuthorization::Manual { initiated_by: None },
        vec![StoredContextOperation::ReasoningSuppression(suppression)],
        None,
        Vec::new(),
    );
    let index = proposed.transactions.len() - 1;
    chat.session.context_view = prepare_context_transition_for_session(
        &chat.provider,
        &chat.session.messages,
        &previous,
        proposed,
        index,
        true,
        "fixture-route",
        None,
        &changed,
    )
    .expect("prepared transition")
    .state;
    let preview = range_over(&chat, "b").expect("shadowing is reported, not refused, in review");
    assert_eq!(preview.shadowed_active_operations, names(&["explicit-b:0"]));
}

#[test]
fn a_transition_under_a_changed_prefix_attributes_only_its_own_blocks() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    // The prefix changed after the last request and no request has
    // reconciled it yet: every block is already invalid before the edit.
    let mut changed = prefix();
    changed.system.push_str(" (instruction reload)");
    changed.recorded_transitions = vec!["agent system replacement".to_string()];
    let previous = chat.session.context_view.clone();
    let summary = chat.summary_of("b", "b");
    let proposed = state_with_transaction(
        &previous,
        "summary",
        1,
        StoredContextAuthorization::Manual { initiated_by: None },
        vec![summary],
        None,
        Vec::new(),
    );
    let staged = stage_for_transition(
        &chat.provider,
        &chat.session.messages,
        &previous,
        proposed,
        &changed,
        "summary",
        StoredContextTransitionKind::Apply,
    )
    .unwrap();
    chat.session.context_view = staged.state;
    let summary = staged.summary.unwrap();
    // `b` is covered by the summary; `a` and `c` were already invalid under
    // the changed prefix, so the edit itself invalidates nothing.
    assert_eq!(chat.suppressed_signatures(), names(&["a", "c"]));
    assert_eq!(summary.invalidated_by_change, 0);
    assert_eq!(summary.invalidated_other, 2);
    assert!(chat.suppressed().iter().all(|(_, cause)| matches!(
        cause,
        StoredReasoningInvalidationCause::RequestPrefixChanged { recorded_transitions }
            if *recorded_transitions == names(&["agent system replacement"])
    )));
}

#[test]
fn a_person_can_select_a_block_jcode_suppresses_and_a_restore_does_not_undo_it() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    chat.apply("summary", vec![summary]);
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c"]));

    // The person suppresses b explicitly; the managed set gives it up.
    let b = jcode_context_core::build_message_range(
        &chat.session.messages,
        chat.index("b"),
        chat.index("b"),
    )
    .unwrap();
    let suppression =
        resolve_reasoning_suppression_for_ranges(&chat.session.messages, &[b]).unwrap();
    chat.apply(
        "explicit-b",
        vec![StoredContextOperation::ReasoningSuppression(suppression)],
    );
    assert_eq!(chat.suppressed_signatures(), names(&["c"]));
    chat.assert_every_replayed_block_is_valid();

    // Reverting the summary would make b valid, but the person's choice
    // stands; c follows a suppressed middle block, so its chain is broken.
    chat.revert("summary");
    assert_eq!(chat.replayed_signatures(), names(&["a"]));
    assert_eq!(chat.suppressed_signatures(), names(&["c"]));
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn people_cannot_revert_or_reapply_a_managed_set_and_undo_skips_it() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    chat.apply("summary", vec![summary]);
    let managed = chat
        .session
        .context_view
        .active_reasoning_invalidation()
        .unwrap()
        .id
        .clone();
    let provider = chat.provider.clone();
    let prefix = prefix();
    let local = || LocalContextRoute {
        provider: &provider,
        route: "fixture-route",
        estimated_total_request_tokens_before: None,
        prefix: &prefix,
    };
    let error = chat
        .service
        .revert_transaction_in_session(&mut chat.session, local(), &managed, false)
        .unwrap_err();
    assert!(
        matches!(error, ContextServiceError::InvalidSelection(_)),
        "{error:?}"
    );
    let error = chat
        .service
        .reapply_transaction_in_session(&mut chat.session, local(), &managed, false)
        .unwrap_err();
    assert!(
        matches!(error, ContextServiceError::InvalidSelection(_)),
        "{error:?}"
    );
    assert_eq!(
        chat.session
            .context_view
            .latest_active_user_transaction()
            .map(|transaction| transaction.id.as_str()),
        Some("summary")
    );
}

#[test]
fn a_rewind_that_removes_managed_targets_is_restaged_with_the_original_cause() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    chat.apply("summary", vec![summary]);
    assert_eq!(chat.suppressed_signatures(), names(&["b", "c"]));

    // Rewind to before c: the managed set's target c disappears.
    let keep = chat.index("c");
    let messages = chat.session.messages[..keep].to_vec();
    let reconciled = jcode_context_core::reconcile_context_after_transcript_edit(
        &messages,
        &chat.session.context_view,
        Utc::now(),
        "rewind",
    )
    .unwrap();
    chat.session.replace_messages(messages);
    chat.session.context_view = reconciled.state;
    assert!(
        chat.session
            .context_view
            .active_reasoning_invalidation()
            .is_none()
    );

    let outcome = reconcile_before_request(
        &chat.provider,
        &chat.session.messages,
        &chat.session.context_view,
        &prefix(),
        &[],
    )
    .unwrap()
    .expect("b is still invalid and must be suppressed again");
    chat.session.context_view = outcome.state;
    assert_eq!(
        chat.suppressed(),
        vec![(
            "b".to_string(),
            transition("summary", StoredContextTransitionKind::Apply)
        )]
    );
    chat.assert_every_replayed_block_is_valid();
}

#[test]
fn a_transcript_without_bound_reasoning_never_reads_the_prefix() {
    struct Unreadable;
    impl super::RequestPrefixSource for Unreadable {
        fn request_prefix(&self) -> Result<ContextRequestPrefix, ContextServiceError> {
            panic!("the prefix must not be composed when nothing binds");
        }
    }
    let mut chat = Conversation::new(true);
    chat.session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "plain".to_string(),
            cache_control: None,
        }],
    );
    let previous = chat.session.context_view.clone();
    let range = jcode_context_core::build_message_range(&chat.session.messages, 0, 1).unwrap();
    let proposed = state_with_transaction(
        &previous,
        "summary",
        1,
        StoredContextAuthorization::Manual { initiated_by: None },
        vec![StoredContextOperation::RangeSummary(
            jcode_session_types::StoredRangeSummary {
                source_range: range,
                summary_text: "summary".to_string(),
                file_change_digest: String::new(),
                changed_files: Vec::new(),
                change_evidence_complete: true,
                file_evidence: None,
                boundary_expansions: Vec::new(),
                generator: None,
                source_token_estimate: 0,
                replacement_token_estimate: 0,
                warnings: Vec::new(),
                created_at: Utc::now(),
                legacy_coverage: None,
            },
        )],
        None,
        Vec::new(),
    );
    let prepared = prepare_context_transition_for_session(
        &chat.provider,
        &chat.session.messages,
        &previous,
        proposed,
        0,
        true,
        "fixture-route",
        None,
        &Unreadable,
    )
    .expect("a plain transcript needs no prefix");
    assert!(prepared.result.reasoning_invalidation.is_none());
}

fn preview_for(
    chat: &Conversation,
    operations: &[StoredContextOperation],
) -> crate::protocol::ContextDraftPreview {
    super::draft::build_preview(super::draft::ContextDraftPreviewInput {
        provider: &chat.provider,
        prefix: &prefix(),
        messages: &chat.session.messages,
        base_state: &chat.session.context_view,
        transaction_id: "draft",
        proposed_revision: if operations.is_empty() {
            chat.session.context_view.revision
        } else {
            chat.session.context_view.revision + 1
        },
        authorization: StoredContextAuthorization::Manual { initiated_by: None },
        operations,
        pricing: None,
        estimated_total_request_tokens_before: None,
        notices: Vec::new(),
        ranges: &[],
        proposals: &[],
    })
    .expect("preview")
}

#[test]
fn the_review_shows_what_apply_will_stage_and_a_no_op_stages_nothing() {
    let mut chat = Conversation::new(true);
    for signature in ["a", "b", "c"] {
        chat.round(signature);
    }
    let summary = chat.summary_of("a", "a");
    let preview = preview_for(&chat, std::slice::from_ref(&summary));
    let reviewed = preview
        .reasoning_invalidation
        .clone()
        .expect("the review lists the invalidated thinking");
    assert_eq!(reviewed.invalidated_by_change, 2);
    assert_eq!(reviewed.restored, 0);

    let applied = chat.apply("draft", vec![summary]).summary.unwrap();
    assert_eq!(
        applied.invalidated_by_change,
        reviewed.invalidated_by_change
    );
    assert_eq!(
        applied.invalidated_by_change_tokens,
        reviewed.invalidated_by_change_tokens
    );
    assert_eq!(
        preview.economics.deleted_input_tokens,
        chat.session
            .context_view
            .transactions
            .iter()
            .find(|transaction| transaction.id == "draft")
            .and_then(|transaction| transaction.economics.as_ref())
            .unwrap()
            .deleted_input_tokens,
        "review and apply compute the same economics"
    );

    let revision = chat.session.context_view.revision;
    let no_op = preview_for(&chat, &[]);
    assert!(no_op.reasoning_invalidation.is_none());
    assert_eq!(no_op.proposed_context_revision, revision);
}

#[test]
fn apply_refuses_a_review_computed_under_another_prefix() {
    let reviewed = Some(prefix().digest());
    super::commit::validate_reviewed_request_prefix(reviewed, &prefix())
        .expect("the same prefix is the reviewed one");
    let mut changed = prefix();
    changed.system.push_str(" (skill)");
    assert!(matches!(
        super::commit::validate_reviewed_request_prefix(reviewed, &changed),
        Err(ContextServiceError::Stale(_))
    ));
    // Reviews on routes that do not bind reasoning carry no digest and never
    // read the prefix.
    struct Unreadable;
    impl super::RequestPrefixSource for Unreadable {
        fn request_prefix(&self) -> Result<ContextRequestPrefix, ContextServiceError> {
            panic!("not read without a reviewed digest");
        }
    }
    super::commit::validate_reviewed_request_prefix(None, &Unreadable).unwrap();
}

#[test]
fn emergency_recovery_stages_the_same_rule_inside_its_authorization() {
    use jcode_session_types::{
        StoredContextEmergencyAudit, StoredContextEmergencyOperationKind,
        StoredContextEmergencyPolicy, StoredContextEmergencyRetryOutcome,
        StoredContextEmergencyTriggerKind,
    };
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", home.path());

    let provider = BindingProvider { binds: true };
    let mut agent = Agent::new(Arc::new(provider.clone()), crate::tool::Registry::empty());
    agent.set_system_prompt(SYSTEM);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "task".to_string(),
            cache_control: None,
        }],
    );
    let agent_prefix = agent.context_request_prefix().expect("agent prefix");
    let mut indices = HashMap::new();
    for signature in ["a", "b", "c"] {
        let projected = super::reasoning_invalidation::request_messages(
            &BindingProvider { binds: true },
            agent.messages(),
            project_context(agent.messages(), agent.context_view_state())
                .unwrap()
                .messages,
        );
        let binding = analyze_request(&fixture_request(
            &projected,
            &agent_prefix.tools,
            &agent_prefix.system,
        ))
        .binding;
        indices.insert(signature, agent.messages().len());
        agent.add_message(
            Role::Assistant,
            vec![
                ContentBlock::AnthropicThinking {
                    thinking: format!("thought {signature}"),
                    signature: signature.to_string(),
                    binding: Some(AnthropicThinkingBinding {
                        model: "claude-sonnet-5-5".to_string(),
                        prefix_digest: binding.prefix_digest,
                        predecessor: binding.last_thinking,
                    }),
                },
                ContentBlock::ToolUse {
                    id: format!("tool-{signature}"),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "true"}),
                    thought_signature: None,
                },
            ],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("tool-{signature}"),
                content: "output ".repeat(200),
                is_error: None,
            }],
        );
    }
    let range =
        jcode_context_core::build_message_range(agent.messages(), indices["a"], indices["a"] + 1)
            .unwrap();
    let summary = StoredContextOperation::RangeSummary(jcode_session_types::StoredRangeSummary {
        source_range: range,
        summary_text: "The oldest round.".to_string(),
        file_change_digest: String::new(),
        changed_files: Vec::new(),
        change_evidence_complete: true,
        file_evidence: None,
        boundary_expansions: Vec::new(),
        generator: None,
        source_token_estimate: 0,
        replacement_token_estimate: 0,
        warnings: Vec::new(),
        created_at: Utc::now(),
        legacy_coverage: None,
    });
    let policy = StoredContextEmergencyPolicy::Authorized {
        protected_recent_assistant_turns: 1,
        target_headroom_percent: 10,
        allow_reasoning_suppression: false,
        allow_tool_distillation: false,
        allow_oldest_range_summary: true,
        authorization_source: "scheduled_item:item-1".to_string(),
    };
    let service = ContextTransactionService::with_persistence_boundaries(
        ContextServiceLimits::default(),
        Arc::new(NoPersistence),
        Arc::new(NoPersistence),
    );
    let result = service
        .apply_unattended_emergency_operations(
            &mut agent,
            "context-emergency-1",
            StoredContextAuthorization::UnattendedEmergency {
                authorization_source: "scheduled_item:item-1".to_string(),
                trigger: Some("preflight_limit".to_string()),
                scheduled_item_id: Some("item-1".to_string()),
            },
            vec![summary],
            Vec::new(),
            StoredContextEmergencyAudit {
                authorization_source: "scheduled_item:item-1".to_string(),
                scheduled_item_id: Some("item-1".to_string()),
                policy,
                trigger_kind: StoredContextEmergencyTriggerKind::PreflightLimit,
                provider_error: None,
                context_window: 1_000_000,
                safe_input_budget: 900_000,
                projected_input_tokens: 950_000,
                required_reduction_to_fit_tokens: 50_000,
                required_reduction_to_target_tokens: 60_000,
                achieved_reduction_tokens: 0,
                protected_recent_assistant_turns: 1,
                protected_message_count: 2,
                operation_order: vec![StoredContextEmergencyOperationKind::OldestRangeSummary],
                retry_outcome: StoredContextEmergencyRetryOutcome::Pending,
            },
        )
        .expect("emergency apply");

    let staged = result
        .reasoning_invalidation
        .expect("the emergency edit invalidated later thinking");
    assert_eq!(staged.invalidated_by_change, 2);
    let state = agent.context_view_state();
    let managed = state.active_reasoning_invalidation().expect("managed set");
    let StoredContextOperation::ReasoningSuppression(suppression) = &managed.operations[0] else {
        panic!("managed set holds suppressions");
    };
    assert_eq!(
        suppression.selection,
        StoredReasoningSelection::Invalidated {
            cause: transition("context-emergency-1", StoredContextTransitionKind::Apply),
        }
    );
    let emergency = state
        .transactions
        .iter()
        .find(|transaction| transaction.id == "context-emergency-1")
        .unwrap();
    let audit = emergency.emergency_audit.as_ref().unwrap();
    assert_eq!(
        audit.achieved_reduction_tokens,
        emergency.economics.as_ref().unwrap().deleted_input_tokens,
        "the recorded reduction includes the suppressed reasoning"
    );
    assert!(audit.achieved_reduction_tokens >= staged.invalidated_by_change_tokens);
}
