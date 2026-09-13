//! Snapshot inspection requests. Content is returned through common retained
//! ToolOutput paging, without exposing storage or provider representations.
use crate::{ToolOutput, presentation::OutputSize};
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "session_inspection_v1";
pub const CLEANUP_CAPABILITY: &str = "output_cleanup_review_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRange {
    /// Inclusive, one-based source message positions from the outline.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspectionRequest {
    Outline {
        target: String,
        #[serde(default)]
        output_size: Option<OutputSize>,
    },
    Transcript {
        snapshot_id: String,
        #[serde(default)]
        range: Option<TranscriptRange>,
        #[serde(default)]
        raw: bool,
        #[serde(default)]
        output_size: Option<OutputSize>,
    },
    ExpandTool {
        snapshot_id: String,
        tool_use_id: String,
        #[serde(default)]
        output_size: Option<OutputSize>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectionResponse {
    pub snapshot_id: String,
    pub content: Box<ToolOutput>,
}
