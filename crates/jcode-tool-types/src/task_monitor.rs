//! Human task browsing over execution receipts, not another execution lifecycle.
use crate::execution::{ExecutionContent, RunRecord};
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "task_monitor_v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskView {
    #[default]
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCursor {
    pub created: i64,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRow {
    pub run: RunRecord,
    pub created: i64,
    pub updated: i64,
    pub child_id: Option<String>,
    pub expandable: bool,
    pub force_stop_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskMonitorRequest {
    List {
        view: TaskView,
        #[serde(default)]
        all_sessions: bool,
        /// Expand a batch's members or a child's complete execution history.
        #[serde(default)]
        parent_run: Option<String>,
        #[serde(default)]
        before: Option<TaskCursor>,
        #[serde(default)]
        limit: Option<u32>,
    },
    Inspect {
        run_id: String,
    },
    /// Bounded text window. Omitted offset selects the most recent window.
    Read {
        run_id: String,
        content: ExecutionContent,
        offset: Option<u64>,
        limit: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTextPage {
    pub start: u64,
    pub end: u64,
    pub total: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskMonitorResponse {
    List {
        rows: Vec<TaskRow>,
        next: Option<TaskCursor>,
    },
    Status {
        row: Box<TaskRow>,
    },
    Text {
        run_id: String,
        content: ExecutionContent,
        page: TaskTextPage,
    },
}
