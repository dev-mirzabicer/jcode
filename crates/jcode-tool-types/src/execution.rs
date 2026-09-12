//! Shared metadata and inspection/control contracts. No SQL or live handles.
use crate::{ProcessExit, RunState, StopCause, ToolOutput, presentation::OutputSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CAPABILITY: &str = "shared_execution_v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionProgress {
    pub value: jcode_background_types::BackgroundTaskProgress,
    pub checkpoint: bool,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_exit: Option<ProcessExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ExecutionProgress>,
    pub id: String,
    pub session_id: String,
    pub message_id: String,
    pub tool: String,
    pub state: RunState,
    pub owner: String,
    pub input_path: PathBuf,
    pub result_path: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub output_bytes: u64,
    pub complete: bool,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub stop_cause: Option<StopCause>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionContent {
    Input,
    Output,
}

/// Explicit human/client requests. These never start or repeat a tool producer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionRequest {
    List {
        #[serde(default)]
        all_sessions: bool,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    Inspect {
        run_id: String,
    },
    Stop {
        run_id: String,
    },
    Background {
        run_id: String,
    },
    Read {
        run_id: String,
        content: ExecutionContent,
        #[serde(default)]
        read_point: Option<String>,
        #[serde(default)]
        output_size: Option<OutputSize>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionResponse {
    List {
        runs: Vec<RunRecord>,
        next: Option<String>,
    },
    Status {
        run: Box<RunRecord>,
    },
    /// Acceptance is not proof that owned work has reached a terminal state.
    Control {
        run_id: String,
        accepted: bool,
        state: RunState,
    },
    Content {
        run_id: String,
        page: Box<ToolOutput>,
    },
}
