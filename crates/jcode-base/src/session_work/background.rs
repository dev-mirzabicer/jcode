//! Background work and scheduled resumes in sessions with session work. A
//! background task or sub-agent run gets its item (alias, label, wake or
//! hold policy) when it starts, and its result always reaches the agent: the
//! arrival rule, not the delivery flags, decides whether it wakes the
//! session. A resume a session schedules for itself gets its `r` item when
//! it is scheduled.
use super::{SessionWorkError, SessionWorkStore};
use chrono::Utc;
use jcode_session_work_types::{FinishPolicy, ItemKind, ItemSource, SessionItem};

const REQUEST_PREFIX: &str = "background:";

/// The item request of a background task.
pub fn request(task_id: &str) -> String {
    format!("{REQUEST_PREFIX}{task_id}")
}

/// The item kind of a background tool.
pub fn kind(tool: &str) -> ItemKind {
    if tool == "subagent" {
        ItemKind::Child
    } else {
        ItemKind::Task
    }
}

/// What a background task's finished arrival names, for its allocation if
/// it was never seen.
pub fn source(task_id: &str, tool: &str, label: &str, wake: bool) -> ItemSource {
    ItemSource {
        request: request(task_id),
        kind: kind(tool),
        label: label.to_string(),
        policy: if wake {
            FinishPolicy::Wake
        } else {
            FinishPolicy::Hold
        },
    }
}

/// Whether `session` has session work. A missing store means no session
/// has it yet; an unreadable one is an error.
pub fn activated(session: &str) -> Result<bool, SessionWorkError> {
    let store = SessionWorkStore::new();
    if !store.exists()? {
        return Ok(false);
    }
    Ok(store.activation(session)?.is_some())
}

/// Allocate a starting background task's item. `None` for a session
/// without session work. The same task returns its original item.
pub fn start(
    session: &str,
    task_id: &str,
    tool: &str,
    label: &str,
    wake: bool,
) -> Result<Option<SessionItem>, SessionWorkError> {
    if !activated(session)? {
        return Ok(None);
    }
    let source = source(task_id, tool, label, wake);
    SessionWorkStore::new()
        .observe_item(
            session,
            &source.request,
            source.kind,
            &source.label,
            source.policy,
            Utc::now(),
        )
        .map(Some)
}

/// The item of a background task, if its session has session work and the
/// item was allocated.
pub fn item(session: &str, task_id: &str) -> Result<Option<SessionItem>, SessionWorkError> {
    if !activated(session)? {
        return Ok(None);
    }
    SessionWorkStore::new().item_for_request(session, &request(task_id))
}

/// The item request of a scheduled resume.
pub fn resume_request(schedule_id: &str) -> String {
    format!("schedule:{schedule_id}")
}

/// What a scheduled resume's arrival names. Resumes always wake.
pub fn resume_source(schedule_id: &str, label: &str) -> ItemSource {
    ItemSource {
        request: resume_request(schedule_id),
        kind: ItemKind::Resume,
        label: label.to_string(),
        policy: FinishPolicy::Wake,
    }
}

/// Allocate a scheduled resume's item for its target session. `None` for a
/// session without session work.
pub fn schedule_resume(
    session: &str,
    schedule_id: &str,
    label: &str,
) -> Result<Option<SessionItem>, SessionWorkError> {
    if !activated(session)? {
        return Ok(None);
    }
    let source = resume_source(schedule_id, label);
    SessionWorkStore::new()
        .observe_item(
            session,
            &source.request,
            source.kind,
            &source.label,
            source.policy,
            Utc::now(),
        )
        .map(Some)
}
