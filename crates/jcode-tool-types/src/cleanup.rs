//! Human cleanup review/confirmation. Not a model tool.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionReport {
    pub checked_at: i64,
    pub archived_outputs: usize,
    pub pruned_snapshots: usize,
    pub resumed_cleanups: usize,
    pub issues: Vec<RetentionIssue>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionIssue {
    pub id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "selection", rename_all = "snake_case", deny_unknown_fields)]
pub enum CleanupSelection {
    OldestBytes { bytes: u64 },
    Outputs { run_ids: Vec<String> },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupCandidate {
    pub run_id: String,
    pub session_id: String,
    pub bytes: u64,
    pub created_at: i64,
    pub affected_snapshot_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReview {
    pub review_id: String,
    pub confirmation_id: String,
    pub candidates: Vec<CleanupCandidate>,
    pub requested_bytes: Option<u64>,
    pub selected_bytes: u64,
    pub overshoot_bytes: u64,
    pub impact: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupItemOutcome {
    pub run_id: String,
    pub deleted: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOutcome {
    pub review_id: String,
    pub items: Vec<CleanupItemOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CleanupRequest {
    Status,
    Review {
        selection: CleanupSelection,
    },
    Confirm {
        review_id: String,
        confirmation_id: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CleanupResponse {
    Status { status: Option<RetentionReport> },
    Review { review: CleanupReview },
    Outcome { outcome: CleanupOutcome },
}
