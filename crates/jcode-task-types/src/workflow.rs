//! Typed workflow render intent. No instruction prose or execution policy.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowPromptRequest {
    StructuredInitial {
        content: String,
        schema: String,
    },
    StructuredCorrection {
        schema: String,
        error_lines: String,
        previous_response: String,
    },
}
