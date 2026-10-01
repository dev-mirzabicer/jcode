//! A provider request that must be planned again before anything is sent.
//!
//! A request is planned once: credential route, model and every
//! model-dependent parameter are resolved, replayed reasoning is reconciled
//! against exactly that plan, and the request is sent unchanged (INT-01/WP-06
//! R20). When a runtime finds the plan no longer holds, it does not patch the
//! request in flight. It returns this error before any output, and the request
//! loop reconciles and builds a new request.

use std::fmt;

/// Why a runtime refused to send, or stopped retrying, a planned request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRequestReplan {
    /// The runtime moved to another model: the selected one is unavailable or
    /// its quota is exhausted. The runtime's model state already names `to`.
    ModelFallback {
        from: String,
        to: String,
        /// Short reason for people ("unavailable", "weekly limit reached").
        cause: String,
    },
    /// The request as it would be sent still replays reasoning the runtime's
    /// own check finds invalid, because the plan differs from the one the
    /// transcript was reconciled against (for example the credential route
    /// resolved differently).
    ReplayedReasoningInvalid { blocks: usize },
    /// The provider rejected replayed reasoning blocks. They must not be sent
    /// again; `block_ids` are in the runtime's `replayed_reasoning_block_id`
    /// scheme.
    ReasoningRejected {
        block_ids: Vec<String>,
        reason: String,
    },
}

impl ProviderRequestReplan {
    /// The replan a provider error carries, if any.
    pub fn of(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref())
    }
}

impl fmt::Display for ProviderRequestReplan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelFallback { from, to, cause } => {
                write!(
                    f,
                    "model '{from}' is {cause}; the request is planned again for '{to}'"
                )
            }
            Self::ReplayedReasoningInvalid { blocks } => write!(
                f,
                "{blocks} replayed reasoning block(s) do not match the request that would be sent; the request is planned again"
            ),
            Self::ReasoningRejected { block_ids, reason } => write!(
                f,
                "the provider rejected {} replayed reasoning block(s) ({reason}); the request is planned again without them",
                block_ids.len()
            ),
        }
    }
}

impl std::error::Error for ProviderRequestReplan {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replan_survives_error_context() {
        let replan = ProviderRequestReplan::ReplayedReasoningInvalid { blocks: 2 };
        let error = anyhow::Error::new(replan.clone()).context("opening the stream");
        assert_eq!(ProviderRequestReplan::of(&error), Some(&replan));
        assert_eq!(ProviderRequestReplan::of(&anyhow::anyhow!("other")), None);
    }
}
