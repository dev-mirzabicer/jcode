use super::*;
use crate::instruction::{
    InstructionRepositoryService, InstructionRuntime, notification::Notification,
};
pub use jcode_session_types::{StoredMessageOrigin, StoredMessagePart, TodoNoticeKind};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RenderedTodoNotice {
    pub text: String,
    pub kind: TodoNoticeKind,
    pub incomplete_count: Option<usize>,
}

/// Complete occurrence-time operation. No source state survives its result.
pub fn render_todo_notice(
    request: &TodoNoticeRequest,
    working_dir: Option<&Path>,
) -> Result<RenderedTodoNotice> {
    let runtime = crate::instruction::notification::occurrence_runtime(
        &InstructionRepositoryService::new(),
        working_dir,
    )?;
    render_todo_notice_in(request, &runtime)
}

pub fn render_todo_notice_in(
    request: &TodoNoticeRequest,
    runtime: &InstructionRuntime,
) -> Result<RenderedTodoNotice> {
    let render = |notice: Notification<'_>| notice.render_in(runtime).map_err(anyhow::Error::new);
    let (text, kind, incomplete_count) = match request {
        TodoNoticeRequest::LongReview => (
            format!(
                "[automated todo assessment review - not a user message] {}",
                render(Notification::TodoLongReview)?
            ),
            TodoNoticeKind::LongReview,
            None,
        ),
        TodoNoticeRequest::Intent => (
            render(Notification::TodoIntent)?,
            TodoNoticeKind::Intent,
            None,
        ),
        TodoNoticeRequest::FeedbackLoop => (
            render(Notification::TodoFeedbackLoop)?,
            TodoNoticeKind::FeedbackLoop,
            None,
        ),
        TodoNoticeRequest::Ownership { todos, goals } => (
            render_ownership(todos, goals, runtime)?,
            TodoNoticeKind::Ownership,
            None,
        ),
        TodoNoticeRequest::Completion { todos } => {
            let completed = todos
                .iter()
                .filter(|todo| todo.status == "completed")
                .collect::<Vec<_>>();
            let needs = completed
                .iter()
                .copied()
                .filter(|todo| !completion_confidence_passes(todo.completion_confidence))
                .collect::<Vec<_>>();
            let targets = if needs.is_empty() { &completed } else { &needs };
            let mut text = followup(render(Notification::TodoCompletion)?);
            append_named_todos(
                &mut text,
                &render(Notification::TodoCompletionLead)?,
                targets,
            );
            (text, TodoNoticeKind::Completion, None)
        }
        TodoNoticeRequest::Confidence { todos } => {
            let mut text = followup(render(Notification::TodoConfidence)?);
            append_named_todos(
                &mut text,
                &render(Notification::TodoConfidenceLead)?,
                &spike_completed_todos(todos),
            );
            (text, TodoNoticeKind::Confidence, None)
        }
        TodoNoticeRequest::Digest {
            observations,
            plan,
            goals,
        } => (
            render_digest(observations, plan, goals, runtime)?,
            TodoNoticeKind::Digest,
            None,
        ),
        TodoNoticeRequest::Incomplete { count } => (
            render(Notification::TodoIncomplete {
                count: *count,
                plural: if *count == 1 { "" } else { "s" },
            })?,
            TodoNoticeKind::Incomplete,
            Some(*count),
        ),
    };
    Ok(RenderedTodoNotice {
        text,
        kind,
        incomplete_count,
    })
}

fn followup(prose: String) -> String {
    format!("[automated follow-up - not a user message] {prose}")
}

fn render_ownership(
    todos: &[TodoItem],
    goals: &[TodoGoal],
    runtime: &InstructionRuntime,
) -> Result<String> {
    let mut groups = Vec::new();
    for todo in todos {
        let group = normalized_group(todo.group.as_deref());
        if group_is_complete(todos, &group) && !groups.contains(&group) {
            groups.push(group);
        }
    }
    let mut message = followup(Notification::TodoOwnership.render_in(runtime)?);
    for group in groups {
        let label = group.as_deref().unwrap_or("ungrouped goal");
        let mut append = |notice: Notification<'_>| -> Result<()> {
            let prose = notice.render_in(runtime)?;
            message.push_str(&format!("\n- Goal \"{label}\": {prose}"));
            Ok(())
        };
        let Some(goal) = goals
            .iter()
            .find(|goal| normalized_group(goal.group.as_deref()) == group)
        else {
            append(Notification::TodoOwnershipClarify)?;
            continue;
        };
        if !goal
            .delivery_state
            .is_some_and(|state| state >= required_delivery_state(goal.difficulty))
        {
            append(Notification::TodoOwnershipDelivery)?;
        }
        if !goal
            .autonomy
            .is_some_and(|state| state >= Autonomy::NecessaryFollowthrough)
        {
            append(Notification::TodoOwnershipAutonomy)?;
        }
        if !goal
            .iteration_maturity
            .is_some_and(IterationMaturity::permits_completion)
        {
            append(Notification::TodoOwnershipIterate)?;
        }
        if !feedback_loop_relevance_passes(goal) {
            append(Notification::TodoOwnershipRelevance)?;
        }
        if !feedback_loop_coverage_passes(goal) {
            append(Notification::TodoOwnershipCoverage)?;
        }
        if !feedback_loop_traceability_passes(goal) {
            append(Notification::TodoOwnershipTraceability)?;
        }
        if matches!(
            goal.iteration_maturity,
            Some(
                IterationMaturity::PlateauConfirmed
                    | IterationMaturity::ConstraintsExhausted
                    | IterationMaturity::BudgetExhausted
            )
        ) && !goal
            .stopping_evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.trim().is_empty())
        {
            append(Notification::TodoOwnershipStop)?;
        }
    }
    Ok(message)
}

fn render_digest(
    observations: &[GateObservation],
    plan: &TodoPlan,
    goals: &[TodoGoal],
    runtime: &InstructionRuntime,
) -> Result<String> {
    let mut points: Vec<(GateObservationKind, Option<String>, usize, bool)> = Vec::new();
    for observation in observations {
        let cleared = observation_score_later_cleared(observation, plan, goals);
        match points
            .iter_mut()
            .find(|(kind, group, _, _)| *kind == observation.kind && *group == observation.group)
        {
            Some((_, _, count, _)) => *count += 1,
            None => points.push((observation.kind, observation.group.clone(), 1, cleared)),
        }
    }
    if points.is_empty() {
        return Ok(String::new());
    }
    let mut message = format!(
        "[automated todo quality review - not a user message] {}",
        Notification::TodoDigest.render_in(runtime)?
    );
    for (kind, group, count, cleared) in points {
        let label = group
            .as_deref()
            .map(|group| format!(" for \"{group}\""))
            .unwrap_or_default();
        let notice = match (kind, cleared) {
            (GateObservationKind::IntentUnderstanding, false) => Notification::TodoDigestIntentOpen,
            (GateObservationKind::IntentUnderstanding, true) => {
                Notification::TodoDigestIntentCleared
            }
            (GateObservationKind::ClosedFeedbackLoop, false) => {
                Notification::TodoDigestLoopOpen { label: &label }
            }
            (GateObservationKind::ClosedFeedbackLoop, true) => {
                Notification::TodoDigestLoopCleared { label: &label }
            }
            (GateObservationKind::FeedbackLoopRelevance, false) => {
                Notification::TodoDigestRelevanceOpen { label: &label }
            }
            (GateObservationKind::FeedbackLoopRelevance, true) => {
                Notification::TodoDigestRelevanceCleared { label: &label }
            }
            (GateObservationKind::FeedbackLoopCoverage, false) => {
                Notification::TodoDigestCoverageOpen { label: &label }
            }
            (GateObservationKind::FeedbackLoopCoverage, true) => {
                Notification::TodoDigestCoverageCleared { label: &label }
            }
            (GateObservationKind::FeedbackLoopTraceability, false) => {
                Notification::TodoDigestTraceabilityOpen { label: &label }
            }
            (GateObservationKind::FeedbackLoopTraceability, true) => {
                Notification::TodoDigestTraceabilityCleared { label: &label }
            }
        };
        let detail = notice.render_in(runtime)?;
        let repeats = if count > 1 {
            format!(" (flagged {count} times this turn)")
        } else {
            String::new()
        };
        message.push_str(&format!("\n- {detail}{repeats}"));
    }
    message.push('\n');
    message.push_str(&Notification::TodoDigestFinish.render_in(runtime)?);
    Ok(message)
}

/// Compose queue entries exactly as the existing double-newline join, with
/// out-of-band origin ranges. Rendering failure returns no partial batch.
pub fn render_queued_messages(
    entries: &[QueuedMessage],
    working_dir: Option<&Path>,
) -> Result<(String, StoredMessageOrigin)> {
    let mut text = String::new();
    let mut parts = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            text.push_str("\n\n");
        }
        let start = text.len();
        let (body, notice, incomplete_count) = match entry {
            QueuedMessage::Current(QueuedMessageContent::Todo { request }) => {
                let rendered = render_todo_notice(request, working_dir)?;
                (
                    rendered.text,
                    Some(rendered.kind),
                    rendered.incomplete_count,
                )
            }
            QueuedMessage::Current(QueuedMessageContent::Human { text }) => {
                (text.clone(), None, None)
            }
            QueuedMessage::Legacy(text) => (text.clone(), legacy_notice_kind(text), None),
        };
        text.push_str(&body);
        parts.push(StoredMessagePart {
            start,
            end: text.len(),
            notice,
            incomplete_count,
        });
    }
    Ok((text, StoredMessageOrigin::Composed(parts)))
}

pub fn legacy_notice_kind(text: &str) -> Option<TodoNoticeKind> {
    if !is_auto_poke_message(text) {
        return None;
    }
    let text = text.trim();
    Some(if text.starts_with(TODO_LONG_SESSION_REVIEW_MESSAGE) {
        TodoNoticeKind::LongReview
    } else if text.starts_with(TODO_GATE_DIGEST_PREFIX) {
        TodoNoticeKind::Digest
    } else if text.starts_with(TODO_CONFIDENCE_SPIKE_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_CONFIDENCE_SPIKE_CONTINUATION_MESSAGE)
    {
        TodoNoticeKind::Confidence
    } else if text.starts_with(TODO_COMPLETION_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_COMPLETION_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_CONFIDENCE_SUMMARY_PREFIX)
    {
        TodoNoticeKind::Completion
    } else if text.starts_with(TODO_OWNERSHIP_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_OWNERSHIP_CONTINUATION_MESSAGE)
    {
        TodoNoticeKind::Ownership
    } else if text.starts_with(TODO_INTENT_UNDERSTANDING_CONTINUATION_MESSAGE) {
        TodoNoticeKind::Intent
    } else if text.starts_with(TODO_CLOSED_FEEDBACK_LOOP_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_HILL_CLIMBABILITY_CONTINUATION_MESSAGE)
        || text.starts_with(LEGACY_TODO_ALIGNMENT_CONTINUATION_MESSAGE)
    {
        TodoNoticeKind::FeedbackLoop
    } else {
        TodoNoticeKind::Incomplete
    })
}

pub fn queued_message_is_todo(entry: &QueuedMessage) -> bool {
    match entry {
        QueuedMessage::Current(QueuedMessageContent::Todo { .. }) => true,
        QueuedMessage::Legacy(text) => is_auto_poke_message(text),
        QueuedMessage::Current(QueuedMessageContent::Human { .. }) => false,
    }
}

/// Human-only queue preview. Never use this as a provider message body.
pub fn queued_message_preview(entry: &QueuedMessage) -> String {
    match entry {
        QueuedMessage::Current(QueuedMessageContent::Human { text })
        | QueuedMessage::Legacy(text) => text.clone(),
        QueuedMessage::Current(QueuedMessageContent::Todo { request }) => {
            let label = match request {
                TodoNoticeRequest::LongReview => "Todo assessment review",
                TodoNoticeRequest::Intent => "Todo intent review",
                TodoNoticeRequest::FeedbackLoop => "Todo feedback-loop review",
                TodoNoticeRequest::Ownership { .. } => "Todo delivery review",
                TodoNoticeRequest::Completion { .. } => "Todo completion review",
                TodoNoticeRequest::Confidence { .. } => "Todo confidence review",
                TodoNoticeRequest::Digest { .. } => "Todo quality review",
                TodoNoticeRequest::Incomplete { count } => {
                    return format!("Continue {count} incomplete todo(s)");
                }
            };
            label.to_string()
        }
    }
}

pub fn todo_notice_display_summary(kind: TodoNoticeKind) -> Option<&'static str> {
    match kind {
        TodoNoticeKind::Confidence => Some("🔍 Double-checking a confidence jump for you..."),
        TodoNoticeKind::Completion => Some("🔍 Double-checking confidence for you..."),
        TodoNoticeKind::Digest => Some("🔍 Reviewing the weak points of this turn for you..."),
        TodoNoticeKind::LongReview => {
            Some("🔍 Rechecking the plan and assessments after extended work...")
        }
        TodoNoticeKind::Ownership => Some("🔍 Checking the delivery state of the finished work..."),
        TodoNoticeKind::Intent => Some("🔍 Re-checking the request was understood..."),
        TodoNoticeKind::FeedbackLoop => Some("🔍 Asking for a stronger way to verify this work..."),
        TodoNoticeKind::Incomplete => None,
    }
}
