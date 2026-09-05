use super::{TodoGoal, TodoItem, TodoPlan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateObservationKind {
    IntentUnderstanding,
    ClosedFeedbackLoop,
    FeedbackLoopRelevance,
    FeedbackLoopCoverage,
    FeedbackLoopTraceability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateObservation {
    pub kind: GateObservationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// One occurrence's data, captured by the existing policy owner. Rendering
/// belongs to the server/runtime, not to a remote client's instruction files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoNoticeRequest {
    LongReview,
    Intent,
    FeedbackLoop,
    Ownership {
        todos: Vec<TodoItem>,
        goals: Vec<TodoGoal>,
    },
    Completion {
        todos: Vec<TodoItem>,
    },
    Confidence {
        todos: Vec<TodoItem>,
    },
    Digest {
        observations: Vec<GateObservation>,
        plan: TodoPlan,
        goals: Vec<TodoGoal>,
    },
    Incomplete {
        count: usize,
    },
}

/// A queue entry is either human text or typed control intent. Legacy string
/// snapshots remain distinguishable so only old queues need prose recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueuedMessage {
    Current(QueuedMessageContent),
    Legacy(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuedMessageContent {
    Human { text: String },
    Todo { request: TodoNoticeRequest },
}

impl From<String> for QueuedMessage {
    fn from(text: String) -> Self {
        Self::Current(QueuedMessageContent::Human { text })
    }
}

impl From<&str> for QueuedMessage {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}

impl QueuedMessage {
    pub fn todo(request: TodoNoticeRequest) -> Self {
        Self::Current(QueuedMessageContent::Todo { request })
    }

    pub fn human_text(&self) -> Option<&str> {
        match self {
            Self::Current(QueuedMessageContent::Human { text }) | Self::Legacy(text) => Some(text),
            Self::Current(QueuedMessageContent::Todo { .. }) => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueuedMessages(Vec<QueuedMessage>);

impl PartialEq<String> for QueuedMessage {
    fn eq(&self, other: &String) -> bool {
        self.human_text() == Some(other.as_str())
    }
}
impl PartialEq<&str> for QueuedMessage {
    fn eq(&self, other: &&str) -> bool {
        self.human_text() == Some(*other)
    }
}
impl<T> PartialEq<Vec<T>> for QueuedMessages
where
    QueuedMessage: PartialEq<T>,
{
    fn eq(&self, other: &Vec<T>) -> bool {
        self.0.len() == other.len() && self.0.iter().zip(other).all(|(left, right)| left == right)
    }
}

impl QueuedMessages {
    pub fn push(&mut self, entry: impl Into<QueuedMessage>) {
        self.0.push(entry.into());
    }
    pub fn insert(&mut self, index: usize, entry: impl Into<QueuedMessage>) {
        self.0.insert(index, entry.into());
    }
    pub fn extend<T: Into<QueuedMessage>>(&mut self, entries: impl IntoIterator<Item = T>) {
        self.0.extend(entries.into_iter().map(Into::into));
    }
    pub fn into_entries(self) -> Vec<QueuedMessage> {
        self.0
    }
}
impl<T: Into<QueuedMessage>> From<Vec<T>> for QueuedMessages {
    fn from(entries: Vec<T>) -> Self {
        Self(entries.into_iter().map(Into::into).collect())
    }
}
impl std::ops::Deref for QueuedMessages {
    type Target = Vec<QueuedMessage>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for QueuedMessages {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl IntoIterator for QueuedMessages {
    type Item = QueuedMessage;
    type IntoIter = std::vec::IntoIter<QueuedMessage>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl<'a> IntoIterator for &'a QueuedMessages {
    type Item = &'a QueuedMessage;
    type IntoIter = std::slice::Iter<'a, QueuedMessage>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
