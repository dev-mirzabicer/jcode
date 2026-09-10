//! Typed workflow render intent. No instruction prose or execution policy.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowPromptRequest {
    Command {
        command: CommandWorkflow,
    },
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
    /// Legacy workflows whose execution contract depends on Swarm.
    pub fn requires_swarm(&self) -> bool {
        match self {
            Self::ReviewStartup { .. } => true,
            Self::Command { command } => command.requires_swarm(),
            Self::StructuredInitial { .. } | Self::StructuredCorrection { .. } => false,
        }
    }
    /// Heap-owned input bytes for diagnostics, not a rendering limit.
    pub fn allocated_bytes(&self) -> usize {
        match self {
            Self::Command { command } => command.allocated_bytes(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLoopMode {
    ImproveRun,
    ImprovePlan,
    RefactorRun,
    RefactorPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTodo {
    pub content: String,
    pub status: String,
    pub priority: String,
}
impl From<&crate::TodoItem> for WorkflowTodo {
    fn from(todo: &crate::TodoItem) -> Self {
        Self {
            content: todo.content.clone(),
            status: todo.status.clone(),
            priority: todo.priority.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandWorkflow {
    Commit,
    CommitPush,
    ReleaseFast,
    ReleaseMacos,
    ReleaseRemote,
    Triage {
        focus: String,
    },
    Test {
        claim: String,
    },
    Plan {
        goal: Option<String>,
    },
    Improve {
        plan_only: bool,
        focus: Option<String>,
    },
    Refactor {
        plan_only: bool,
        focus: Option<String>,
    },
    ImproveStop,
    RefactorStop,
    ImproveResume {
        mode: WorkflowLoopMode,
        todos: Vec<WorkflowTodo>,
    },
    RefactorResume {
        mode: WorkflowLoopMode,
        todos: Vec<WorkflowTodo>,
    },
}
impl CommandWorkflow {
    pub fn requires_swarm(&self) -> bool {
        matches!(
            self,
            Self::Triage { .. }
                | Self::Refactor { .. }
                | Self::RefactorStop
                | Self::RefactorResume { .. }
                | Self::ImproveResume {
                    mode: WorkflowLoopMode::RefactorRun | WorkflowLoopMode::RefactorPlan,
                    ..
                }
        )
    }
    pub fn allocated_bytes(&self) -> usize {
        match self {
            Self::Triage { focus } => focus.capacity(),
            Self::Test { claim } => claim.capacity(),
            Self::Plan { goal } => goal.as_ref().map_or(0, String::capacity),
            Self::Improve { focus, .. } | Self::Refactor { focus, .. } => {
                focus.as_ref().map_or(0, String::capacity)
            }
            Self::ImproveResume { todos, .. } | Self::RefactorResume { todos, .. } => {
                todos.iter().fold(
                    todos
                        .capacity()
                        .saturating_mul(std::mem::size_of::<WorkflowTodo>()),
                    |sum, todo| {
                        sum.saturating_add(todo.content.capacity())
                            .saturating_add(todo.status.capacity())
                            .saturating_add(todo.priority.capacity())
                    },
                )
            }
            _ => 0,
        }
    }
}
