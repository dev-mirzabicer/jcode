use crate::message::Message;
use crate::provider::{
    ContextProjectionValidationOperation, ContextProjectionValidationReport, Provider,
};
use jcode_session_types::StoredContextViewState;
use std::fmt;

/// Run provider-specific projected-history validation without network I/O.
///
/// The active runtime remains the authority. Its implementation must invoke the same production
/// request builder used by `Provider::complete`; app core never duplicates provider serialization.
pub fn validate_projected_messages(
    provider: &dyn Provider,
    messages: &[Message],
    operations: &[ContextProjectionValidationOperation],
) -> ContextProjectionValidationReport {
    provider.validate_projected_context(messages, operations)
}

/// Actionable error returned when a projected history cannot be committed for the active route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedProjectedContext {
    pub report: Box<ContextProjectionValidationReport>,
}

impl fmt::Display for UnsupportedProjectedContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reasons = self.report.unsupported_reasons();
        if reasons.is_empty() {
            write!(
                formatter,
                "Projected context is unsupported by {} model {}.",
                self.report.provider_display_name, self.report.model
            )
        } else {
            write!(
                formatter,
                "Projected context is unsupported by {} model {}: {}",
                self.report.provider_display_name,
                self.report.model,
                reasons.join("; ")
            )
        }
    }
}

impl std::error::Error for UnsupportedProjectedContext {}

/// Require a fully supported provider validation result before transaction activation.
pub fn require_supported_projected_messages(
    provider: &dyn Provider,
    messages: &[Message],
    operations: &[ContextProjectionValidationOperation],
) -> Result<ContextProjectionValidationReport, UnsupportedProjectedContext> {
    let report = validate_projected_messages(provider, messages, operations);
    if report.is_supported() {
        Ok(report)
    } else {
        Err(UnsupportedProjectedContext {
            report: Box::new(report),
        })
    }
}

/// Validate every active persisted operation against a candidate provider/model identity.
/// A raw/default view has no provider-sensitive transform and therefore needs no adapter gate.
pub fn require_supported_context_view(
    provider: &dyn Provider,
    messages: &[Message],
    state: &StoredContextViewState,
) -> Result<Option<ContextProjectionValidationReport>, UnsupportedProjectedContext> {
    let operations = super::draft::projection_validation_operations(state);
    if operations.is_empty() {
        return Ok(None);
    }
    require_supported_projected_messages(provider, messages, &operations).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{StreamEvent, ToolDefinition};
    use crate::provider::{
        ContextProjectionOperationKind, ContextProviderFamily, ContextProviderValidationIdentity,
        ContextRequestBuilderValidation, EventStream, context_projection_validation_report,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct NoAdapterProvider;

    #[async_trait]
    impl Provider for NoAdapterProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            let stream = futures::stream::empty::<Result<StreamEvent>>();
            Ok(Box::pin(stream))
        }

        fn name(&self) -> &str {
            "no-adapter"
        }

        fn model(&self) -> String {
            "test-model".to_string()
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
    }

    struct ValidatingProvider;

    #[async_trait]
    impl Provider for ValidatingProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            let stream = futures::stream::empty::<Result<StreamEvent>>();
            Ok(Box::pin(stream))
        }

        fn name(&self) -> &str {
            "validating"
        }

        fn model(&self) -> String {
            "test-model".to_string()
        }

        fn validate_projected_context(
            &self,
            messages: &[Message],
            operations: &[ContextProjectionValidationOperation],
        ) -> ContextProjectionValidationReport {
            context_projection_validation_report(
                ContextProviderValidationIdentity {
                    family: ContextProviderFamily::OpenAiResponses,
                    provider_name: self.name().to_string(),
                    provider_display_name: self.display_name(),
                    model: self.model(),
                    evidence_tag: "test_real_builder_v1".to_string(),
                },
                operations,
                None,
                Ok(ContextRequestBuilderValidation::new(messages.len())),
            )
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
    }

    #[test]
    fn default_provider_adapter_fails_closed_with_actionable_identity() {
        let provider = NoAdapterProvider;
        let operation = ContextProjectionValidationOperation {
            id: "summary-1".to_string(),
            kind: ContextProjectionOperationKind::RangeSummary,
        };
        let error = require_supported_projected_messages(
            &provider,
            &[Message::user("projected")],
            &[operation],
        )
        .unwrap_err();

        assert_eq!(error.report.provider_name, "no-adapter");
        assert_eq!(error.report.model, "test-model");
        assert!(
            error
                .to_string()
                .contains("no production request-builder validation adapter")
        );
    }

    #[test]
    fn supported_adapter_returns_evidence_without_raw_content() {
        let provider = ValidatingProvider;
        let report = require_supported_projected_messages(
            &provider,
            &[Message::user("sensitive projected content")],
            &[ContextProjectionValidationOperation {
                id: "summary-1".to_string(),
                kind: ContextProjectionOperationKind::RangeSummary,
            }],
        )
        .expect("validation must pass");

        assert!(report.is_supported());
        assert_eq!(report.normalized_item_count, 1);
        assert_eq!(report.evidence_tag, "test_real_builder_v1");
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("sensitive projected content"));
    }
}
