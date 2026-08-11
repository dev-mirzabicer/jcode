use crate::{build_content_target, build_message_range, project_context};
use chrono::Utc;
use jcode_message_types::{ContentBlock, Role};
use jcode_session_types::{
    StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextOperation,
    StoredContextStatusEvent, StoredContextTransaction, StoredContextTransactionStatusKind,
    StoredContextViewState, StoredMessage, StoredRangeSummary, StoredReasoningSelection,
    StoredReasoningSuppression, StoredToolResultDistillation,
};
use serde_json::json;

fn stored(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        id: id.to_string(),
        role,
        content,
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn text(id: &str, role: Role, value: &str) -> StoredMessage {
    stored(
        id,
        role,
        vec![ContentBlock::Text {
            text: value.to_string(),
            cache_control: None,
        }],
    )
}

fn generator() -> StoredContextArtifactGenerator {
    StoredContextArtifactGenerator {
        provider: "curator".to_string(),
        model: "test-model".to_string(),
        route: "test-route".to_string(),
        prompt_version: "provider-validation-v1".to_string(),
        effort: None,
    }
}

fn applied_state(operations: Vec<StoredContextOperation>) -> StoredContextViewState {
    StoredContextViewState {
        revision: 1,
        transactions: vec![StoredContextTransaction {
            id: "provider-validation-transaction".to_string(),
            base_revision: 0,
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations,
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
        }],
        ..StoredContextViewState::default()
    }
}

fn raw_tool_fixture() -> Vec<StoredMessage> {
    vec![
        text("user-1", Role::User, "Inspect Cargo.toml."),
        stored(
            "assistant-tool",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "generic replay reasoning".to_string(),
                },
                ContentBlock::AnthropicThinking {
                    thinking: "signed Anthropic thinking".to_string(),
                    signature: "anthropic-signature".to_string(),
                },
                ContentBlock::OpenAIReasoning {
                    id: "rs_projected".to_string(),
                    summary: vec!["OpenAI reasoning summary".to_string()],
                    encrypted_content: Some("encrypted-state".to_string()),
                    status: Some("completed".to_string()),
                },
                ContentBlock::ToolUse {
                    id: "call_read".to_string(),
                    name: "read".to_string(),
                    input: json!({"file_path":"Cargo.toml"}),
                    thought_signature: Some("gemini-thought-signature".to_string()),
                },
            ],
        ),
        stored(
            "tool-result",
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call_read".to_string(),
                content: "large original tool output".repeat(100),
                is_error: None,
            }],
        ),
        text(
            "assistant-final",
            Role::Assistant,
            "The provider-neutral result is sufficient.",
        ),
        text("user-2", Role::User, "Continue."),
    ]
}

#[test]
fn combined_real_projection_is_accepted_by_every_primary_structured_provider_builder() {
    let raw = raw_tool_fixture();
    let raw_before = serde_json::to_vec(&raw).unwrap();
    let reasoning_targets = [0usize, 1, 2]
        .into_iter()
        .map(|block_index| build_content_target(&raw, 1, block_index).unwrap())
        .collect::<Vec<_>>();
    let suppression = StoredReasoningSuppression {
        selection: StoredReasoningSelection::MessageRanges { ranges: Vec::new() },
        assistant_turns_affected: 1,
        targets: reasoning_targets.clone(),
        replay_block_kinds: reasoning_targets.iter().map(|target| target.kind).collect(),
        original_token_estimate: 300,
        validation_evidence_version: 1,
        validation: Vec::new(),
    };
    let result_target = build_content_target(&raw, 2, 0).unwrap();
    let distillation = StoredToolResultDistillation {
        target: result_target,
        tool_name: "read".to_string(),
        tool_call_id: "call_read".to_string(),
        replacement_content: "Cargo.toml was read successfully; package metadata was preserved."
            .to_string(),
        original_token_estimate: 1_000,
        replacement_token_estimate: 100,
        replacement_ratio_millionths: 100_000,
        preservation_rationale: "The concise result preserves every fact needed later.".to_string(),
        uncertainties: Vec::new(),
        generator: generator(),
        created_at: Utc::now(),
    };
    let state = applied_state(vec![
        StoredContextOperation::ReasoningSuppression(suppression),
        StoredContextOperation::ToolResultDistillation(distillation),
    ]);

    let projection = project_context(&raw, &state).expect("combined projection");
    assert_eq!(serde_json::to_vec(&raw).unwrap(), raw_before);
    assert!(projection.messages.iter().all(|message| {
        message.content.iter().all(|block| {
            !matches!(
                block,
                ContentBlock::Reasoning { .. }
                    | ContentBlock::AnthropicThinking { .. }
                    | ContentBlock::OpenAIReasoning { .. }
            )
        })
    }));

    jcode_provider_anthropic::validate_projected_messages(&projection.messages, false)
        .expect("Anthropic builder");
    jcode_provider_openai::validate_projected_messages(&projection.messages)
        .expect("OpenAI Responses builder");
    let openrouter = jcode_provider_openrouter::request::validate_projected_messages(
        &projection.messages,
        true,
        true,
        true,
    )
    .expect("OpenRouter builder");
    assert_eq!(openrouter.formatter_placeholder_count, 1);
    jcode_provider_gemini::validate_projected_messages(&projection.messages)
        .expect("Gemini builder with preserved thought signature");
}

#[test]
fn real_range_summary_projection_replacing_complete_tool_pair_is_provider_valid() {
    let raw = raw_tool_fixture();
    let summary = StoredRangeSummary {
        source_range: build_message_range(&raw, 1, 2).expect("closed tool range"),
        summary_text: "The assistant read Cargo.toml and the tool returned valid package metadata."
            .to_string(),
        file_change_digest: "No files were changed.".to_string(),
        changed_files: Vec::new(),
        change_evidence_complete: true,
        boundary_expansions: Vec::new(),
        generator: Some(generator()),
        source_token_estimate: 2_000,
        replacement_token_estimate: 120,
        warnings: Vec::new(),
        created_at: Utc::now(),
        legacy_coverage: None,
    };
    let state = applied_state(vec![StoredContextOperation::RangeSummary(summary)]);
    let projection = project_context(&raw, &state).expect("range summary projection");

    assert!(projection.messages.iter().all(|message| {
        message.content.iter().all(|block| {
            !matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            )
        })
    }));
    jcode_provider_anthropic::validate_projected_messages(&projection.messages, true)
        .expect("Anthropic summary projection");
    jcode_provider_openai::validate_projected_messages(&projection.messages)
        .expect("OpenAI summary projection");
    jcode_provider_openrouter::request::validate_projected_messages(
        &projection.messages,
        false,
        false,
        true,
    )
    .expect("strict OpenAI-compatible summary projection");
    jcode_provider_gemini::validate_projected_messages(&projection.messages)
        .expect("Gemini summary projection");
}
