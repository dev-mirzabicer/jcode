use serde::{Deserialize, Serialize};

/// Provider request family whose production formatter validated a projected history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProviderFamily {
    Anthropic,
    OpenAiResponses,
    OpenRouterCompatible,
    Gemini,
    BedrockConverse,
    CursorAgent,
    Unknown,
}

impl ContextProviderFamily {
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic Messages",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::OpenRouterCompatible => "OpenRouter/OpenAI-compatible chat",
            Self::Gemini => "Gemini generateContent",
            Self::BedrockConverse => "Amazon Bedrock Converse",
            Self::CursorAgent => "Cursor Agent text prompt",
            Self::Unknown => "unknown provider",
        }
    }
}

/// Reasoning-like block selected by an ephemeral context-validation request.
///
/// These values are not persisted transaction copies. They describe only the
/// operation currently being checked against an active provider request builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReasoningBlockKind {
    GenericReasoning,
    AnthropicThinking,
    OpenAiReasoning,
    ReasoningTrace,
    ToolUseThoughtSignature,
}

impl ContextReasoningBlockKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GenericReasoning => "generic replay reasoning",
            Self::AnthropicThinking => "Anthropic signed thinking",
            Self::OpenAiReasoning => "OpenAI Responses reasoning",
            Self::ReasoningTrace => "history-only reasoning trace",
            Self::ToolUseThoughtSignature => "Gemini tool-use thought signature",
        }
    }
}

/// One provider-neutral operation in a projected-history validation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextProjectionOperationKind {
    RangeSummary,
    ReasoningSuppression {
        block_kind: ContextReasoningBlockKind,
    },
    ToolResultDistillation,
}

impl ContextProjectionOperationKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::RangeSummary => "selected range summary",
            Self::ReasoningSuppression { .. } => "reasoning suppression",
            Self::ToolResultDistillation => "tool-result distillation",
        }
    }
}

/// Stable operation identity supplied by the draft service when validation is run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextProjectionValidationOperation {
    pub id: String,
    pub kind: ContextProjectionOperationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProjectionValidationStatus {
    Supported,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProjectionValidationStage {
    ProviderSelection,
    OperationSupport,
    RequestBuilder,
    RequestNormalization,
}

/// One actionable validation finding. Raw projected content is deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextProjectionValidationFinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<ContextProjectionOperationKind>,
    pub status: ContextProjectionValidationStatus,
    pub stage: ContextProjectionValidationStage,
    pub reason: String,
}

/// Sensitive-content-free summary returned by a production request-builder adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequestBuilderValidation {
    pub normalized_item_count: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub formatter_placeholder_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalization_notes: Vec<String>,
}

const fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl ContextRequestBuilderValidation {
    pub fn new(normalized_item_count: usize) -> Self {
        Self {
            normalized_item_count,
            formatter_placeholder_count: 0,
            normalization_notes: Vec::new(),
        }
    }
}

/// Provider identity captured at validation time for later review and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextProviderValidationIdentity {
    pub family: ContextProviderFamily,
    pub provider_name: String,
    pub provider_display_name: String,
    pub model: String,
    pub evidence_tag: String,
}

/// Complete provider validation result for one projected provider view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextProjectionValidationReport {
    pub provider_family: ContextProviderFamily,
    pub provider_name: String,
    pub provider_display_name: String,
    pub model: String,
    pub evidence_tag: String,
    pub builder_status: ContextProjectionValidationStatus,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub normalized_item_count: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub formatter_placeholder_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalization_notes: Vec<String>,
    pub findings: Vec<ContextProjectionValidationFinding>,
}

impl ContextProjectionValidationReport {
    pub fn is_supported(&self) -> bool {
        self.builder_status == ContextProjectionValidationStatus::Supported
            && self
                .findings
                .iter()
                .all(|finding| finding.status == ContextProjectionValidationStatus::Supported)
    }

    pub fn unsupported_reasons(&self) -> Vec<&str> {
        self.findings
            .iter()
            .filter(|finding| finding.status == ContextProjectionValidationStatus::Unsupported)
            .map(|finding| finding.reason.as_str())
            .collect()
    }
}

/// Build the common report while preserving provider-specific formatter evidence.
///
/// Range summaries and tool-result distillations are provider-neutral once the
/// real builder accepts the projected shape. Replayed reasoning is supported only
/// when its block kind matches the active request family. Trace-only reasoning and
/// Gemini function-call signatures are never valid suppression targets.
pub fn context_projection_validation_report(
    identity: ContextProviderValidationIdentity,
    operations: &[ContextProjectionValidationOperation],
    supported_reasoning_kind: Option<ContextReasoningBlockKind>,
    builder_result: Result<ContextRequestBuilderValidation, String>,
) -> ContextProjectionValidationReport {
    let (builder_status, builder, builder_reason) = match builder_result {
        Ok(builder) => (
            ContextProjectionValidationStatus::Supported,
            builder,
            format!(
                "Projected history passed the production {} request builder.",
                identity.family.label()
            ),
        ),
        Err(reason) => (
            ContextProjectionValidationStatus::Unsupported,
            ContextRequestBuilderValidation::new(0),
            reason,
        ),
    };

    let mut findings = vec![ContextProjectionValidationFinding {
        operation_id: None,
        operation_kind: None,
        status: builder_status,
        stage: ContextProjectionValidationStage::RequestBuilder,
        reason: builder_reason,
    }];

    for operation in operations {
        let (status, reason) = match &operation.kind {
            ContextProjectionOperationKind::RangeSummary => (
                ContextProjectionValidationStatus::Supported,
                format!(
                    "Selected range summary is supported by the {} request builder.",
                    identity.family.label()
                ),
            ),
            ContextProjectionOperationKind::ToolResultDistillation => (
                ContextProjectionValidationStatus::Supported,
                format!(
                    "Tool-result distillation preserves the provider-visible call/result structure accepted by the {} request builder.",
                    identity.family.label()
                ),
            ),
            ContextProjectionOperationKind::ReasoningSuppression { block_kind } => {
                if *block_kind == ContextReasoningBlockKind::ReasoningTrace {
                    (
                        ContextProjectionValidationStatus::Unsupported,
                        "ReasoningTrace is history-only and is already omitted from every provider request; suppressing it cannot recover provider tokens."
                            .to_string(),
                    )
                } else if *block_kind == ContextReasoningBlockKind::ToolUseThoughtSignature {
                    (
                        ContextProjectionValidationStatus::Unsupported,
                        "ToolUse.thought_signature is provider-required Gemini function-call state, not suppressible reasoning."
                            .to_string(),
                    )
                } else if supported_reasoning_kind == Some(*block_kind) {
                    (
                        ContextProjectionValidationStatus::Supported,
                        format!(
                            "{} suppression is supported by the active {} request builder.",
                            block_kind.label(),
                            identity.family.label()
                        ),
                    )
                } else {
                    (
                        ContextProjectionValidationStatus::Unsupported,
                        format!(
                            "{} is not replayed by the active {} request builder and cannot be validated as a meaningful provider-context reduction.",
                            block_kind.label(),
                            identity.family.label()
                        ),
                    )
                }
            }
        };
        findings.push(ContextProjectionValidationFinding {
            operation_id: Some(operation.id.clone()),
            operation_kind: Some(operation.kind.clone()),
            status,
            stage: ContextProjectionValidationStage::OperationSupport,
            reason,
        });
    }

    ContextProjectionValidationReport {
        provider_family: identity.family,
        provider_name: identity.provider_name,
        provider_display_name: identity.provider_display_name,
        model: identity.model,
        evidence_tag: identity.evidence_tag,
        builder_status,
        normalized_item_count: builder.normalized_item_count,
        formatter_placeholder_count: builder.formatter_placeholder_count,
        normalization_notes: builder.normalization_notes,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ContextProviderValidationIdentity {
        ContextProviderValidationIdentity {
            family: ContextProviderFamily::OpenRouterCompatible,
            provider_name: "openrouter".to_string(),
            provider_display_name: "OpenRouter".to_string(),
            model: "test-model".to_string(),
            evidence_tag: "test-builder-v1".to_string(),
        }
    }

    #[test]
    fn report_rejects_trace_and_thought_signature_suppression() {
        let operations = vec![
            ContextProjectionValidationOperation {
                id: "trace".to_string(),
                kind: ContextProjectionOperationKind::ReasoningSuppression {
                    block_kind: ContextReasoningBlockKind::ReasoningTrace,
                },
            },
            ContextProjectionValidationOperation {
                id: "signature".to_string(),
                kind: ContextProjectionOperationKind::ReasoningSuppression {
                    block_kind: ContextReasoningBlockKind::ToolUseThoughtSignature,
                },
            },
        ];
        let report = context_projection_validation_report(
            identity(),
            &operations,
            Some(ContextReasoningBlockKind::GenericReasoning),
            Ok(ContextRequestBuilderValidation::new(2)),
        );

        assert!(!report.is_supported());
        assert_eq!(report.unsupported_reasons().len(), 2);
        assert!(report.unsupported_reasons()[0].contains("history-only"));
        assert!(report.unsupported_reasons()[1].contains("provider-required"));
    }

    #[test]
    fn report_keeps_formatter_placeholders_separate_from_meaningful_reasoning() {
        let operations = vec![ContextProjectionValidationOperation {
            id: "generic".to_string(),
            kind: ContextProjectionOperationKind::ReasoningSuppression {
                block_kind: ContextReasoningBlockKind::GenericReasoning,
            },
        }];
        let report = context_projection_validation_report(
            identity(),
            &operations,
            Some(ContextReasoningBlockKind::GenericReasoning),
            Ok(ContextRequestBuilderValidation {
                normalized_item_count: 3,
                formatter_placeholder_count: 1,
                normalization_notes: vec![
                    "One required one-space reasoning_content placeholder was inserted."
                        .to_string(),
                ],
            }),
        );

        assert!(report.is_supported());
        assert_eq!(report.formatter_placeholder_count, 1);
        assert_eq!(report.normalization_notes.len(), 1);
    }
}
