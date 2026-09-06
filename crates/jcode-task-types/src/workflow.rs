//! Typed workflow render intent. No instruction prose or execution policy.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowPromptRequest {
    ReviewStartup {
        mode: ReviewWorkflowKind,
        parent_session_id: String,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewWorkflowKind {
    Review,
    Autoreview,
    Judge,
    Autojudge,
}

impl WorkflowPromptRequest {
    /// Heap-owned input bytes for diagnostics, not a rendering limit.
    pub fn allocated_bytes(&self) -> usize {
        match self {
            Self::ReviewStartup {
                parent_session_id, ..
            } => parent_session_id.capacity(),
            Self::StructuredInitial { content, schema } => {
                content.capacity().saturating_add(schema.capacity())
            }
            Self::StructuredCorrection {
                schema,
                error_lines,
                previous_response,
            } => schema
                .capacity()
                .saturating_add(error_lines.capacity())
                .saturating_add(previous_response.capacity()),
        }
    }
}
