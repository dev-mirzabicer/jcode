//! The stop protocol's rules: building and validating declarations against
//! the state they name, the end-of-turn decision, and the correction
//! counters. Callers supply the running background work they observe; the
//! session-work store records every decision.
use super::{SessionWorkError, SessionWorkStore};
use chrono::{DateTime, Utc};
use jcode_session_work_types::{
    CorrectionLimits, Declaration, DeclarationState, ExhaustedCounter, FinishPolicy, ItemAlias,
    ItemKind, ModuleStatus, SessionItem, SessionWorkRole, StopRecord, TimeoutSource, TurnOutcome,
    WaitCondition, WaitTimeout, WaitTimeoutDefaults,
};

/// One piece of background work the session owns, as its owner reports it.
#[derive(Clone, Debug)]
pub struct BackgroundWork {
    pub task_id: String,
    pub kind: ItemKind,
    pub label: String,
    pub wake: bool,
    pub running: bool,
}

/// An item with whether its work is still running. Items without a running
/// owner have reached a final state.
#[derive(Clone, Debug)]
pub struct ObservedItem {
    pub item: SessionItem,
    pub running: bool,
}

const BACKGROUND_REQUEST: &str = "background:";

/// Give every running piece of background work an alias, and report all of
/// the session's items with their running state. A task's alias and policy
/// are fixed when it is first seen.
pub fn observe_items(
    store: &SessionWorkStore,
    session: &str,
    work: &[BackgroundWork],
    now: DateTime<Utc>,
) -> Result<Vec<ObservedItem>, SessionWorkError> {
    for task in work.iter().filter(|task| task.running) {
        store.observe_item(
            session,
            &format!("{BACKGROUND_REQUEST}{}", task.task_id),
            task.kind,
            &task.label,
            if task.wake {
                FinishPolicy::Wake
            } else {
                FinishPolicy::Hold
            },
            now,
        )?;
    }
    Ok(store
        .items_with_requests(session)?
        .into_iter()
        .map(|(item, request)| {
            let running = request
                .strip_prefix(BACKGROUND_REQUEST)
                .is_some_and(|task| work.iter().any(|w| w.task_id == task && w.running));
            ObservedItem { item, running }
        })
        .collect())
}

/// What `declare_stop` was asked to do, before validation.
#[derive(Clone, Debug, Default)]
pub struct DeclareInput {
    pub action: String,
    pub condition: Option<String>,
    pub timeout: Option<String>,
    pub summary: Option<String>,
    pub leave_running: Vec<String>,
    pub closeout: bool,
    pub documents: Vec<String>,
    pub accepted: bool,
}

/// Why a declaration was refused. Callers render each with its approved text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// Completion with unfinished modules: `(id, status)` in file order.
    Unfinished(Vec<(String, ModuleStatus)>),
    /// Completion while background work runs outside `leave_running`.
    StillRunning(Vec<SessionItem>),
    /// Anything else, as one sentence ending with a period.
    Invalid(String),
}

/// What a `declare_stop` call did.
#[derive(Clone, Debug)]
pub enum DeclareOutcome {
    /// A wait or completion was declared. `also_running` lists running items
    /// outside a wait's condition.
    Declared {
        record: StopRecord,
        also_running: Vec<SessionItem>,
    },
    Withdrawn(StopRecord),
    Refused(Refusal),
}

fn invalid(reason: impl Into<String>) -> Refusal {
    Refusal::Invalid(reason.into())
}

/// Parse and check a declaration's arguments. Nothing is stored.
pub fn build_declaration(
    input: &DeclareInput,
    role: SessionWorkRole,
    defaults: &WaitTimeoutDefaults,
) -> Result<Declaration, Refusal> {
    let summary = input
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty());
    if summary.is_some_and(|summary| summary.contains('\n')) {
        return Err(invalid("the summary must be one line."));
    }
    match (input.action.as_str(), role) {
        ("wait", SessionWorkRole::Primary) => {
            let condition = input
                .condition
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| {
                    invalid("a wait needs a condition, such as `q1` or `any(q1, t3)`.")
                })?;
            let condition: WaitCondition = condition
                .parse()
                .map_err(|error| invalid(format!("{error}.")))?;
            let summary = summary
                .ok_or_else(|| invalid("a wait needs a one-line summary for Mirza's lists."))?;
            let (timeout, timeout_source) = match input.timeout.as_deref() {
                Some(text) => (
                    text.parse::<WaitTimeout>()
                        .map_err(|error| invalid(format!("{error}.")))?,
                    TimeoutSource::Explicit,
                ),
                None => (defaults.for_condition(&condition), TimeoutSource::Default),
            };
            Ok(Declaration::Wait {
                condition,
                timeout,
                timeout_source,
                summary: summary.to_string(),
            })
        }
        ("complete", _) => {
            let mut leave_running = Vec::new();
            for text in &input.leave_running {
                let alias: ItemAlias = text
                    .trim()
                    .parse()
                    .map_err(|error| invalid(format!("leave_running: {error}.")))?;
                if !leave_running.contains(&alias) {
                    leave_running.push(alias);
                }
            }
            Ok(Declaration::Complete {
                summary: summary.map(str::to_string),
                leave_running,
                closeout: input.closeout,
                documents: input.documents.clone(),
                accepted: input.accepted,
            })
        }
        ("wait" | "withdraw", SessionWorkRole::Child) => Err(invalid(
            "an isolated child declares only `complete`; your reply goes to your parent.",
        )),
        (other, _) => Err(invalid(format!(
            "`{other}` is not an action; use wait, complete or withdraw."
        ))),
    }
}

/// Check a declaration against the state it names now.
pub fn validate(
    store: &SessionWorkStore,
    session: &str,
    declaration: &Declaration,
    items: &[ObservedItem],
) -> Result<Result<(), Refusal>, SessionWorkError> {
    let running: Vec<&SessionItem> = items
        .iter()
        .filter(|observed| observed.running)
        .map(|observed| &observed.item)
        .collect();
    let running_list = || {
        if running.is_empty() {
            "nothing is running".to_string()
        } else {
            format!(
                "running items are {}",
                running
                    .iter()
                    .map(|item| item.alias.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    };
    match declaration {
        Declaration::Wait { condition, .. } => {
            for alias in condition.aliases() {
                if !items.iter().any(|observed| observed.item.alias == alias) {
                    return Ok(Err(invalid(format!(
                        "{alias} is not one of this session's items; {}.",
                        running_list()
                    ))));
                }
            }
            let finished = |alias: ItemAlias| {
                items
                    .iter()
                    .any(|observed| observed.item.alias == alias && !observed.running)
            };
            if condition.is_met(&finished) {
                let done: Vec<String> = condition
                    .aliases()
                    .into_iter()
                    .filter(|alias| finished(*alias))
                    .map(|alias| alias.to_string())
                    .collect();
                return Ok(Err(invalid(if done.len() == 1 {
                    format!(
                        "{} already finished, and its result is in your conversation.",
                        done[0]
                    )
                } else {
                    format!(
                        "{} already finished, and their results are in your conversation.",
                        join_and(&done)
                    )
                })));
            }
            Ok(Ok(()))
        }
        Declaration::Complete { leave_running, .. } => {
            if let Some(head) = store.workflow_head(session)? {
                let workflow = super::parse_workflow(&head.revision.text).map_err(|errors| {
                    SessionWorkError::Corrupt(format!(
                        "stored workflow revision {} no longer parses: {errors}",
                        head.revision.revision
                    ))
                })?;
                let unfinished: Vec<(String, ModuleStatus)> = workflow
                    .unfinished()
                    .into_iter()
                    .map(|module| (module.id.as_str().to_string(), module.status))
                    .collect();
                if !unfinished.is_empty() {
                    return Ok(Err(Refusal::Unfinished(unfinished)));
                }
            }
            for alias in leave_running {
                if !running.iter().any(|item| item.alias == *alias) {
                    return Ok(Err(invalid(format!(
                        "leave_running names {alias}, which is not running; {}.",
                        running_list()
                    ))));
                }
            }
            let still: Vec<SessionItem> = running
                .iter()
                .filter(|item| !leave_running.contains(&item.alias))
                .map(|item| (*item).clone())
                .collect();
            if !still.is_empty() {
                return Ok(Err(Refusal::StillRunning(still)));
            }
            Ok(Ok(()))
        }
    }
}

/// Handle one `declare_stop` call. `request` is the invocation identity, so
/// a replayed call converges on its original record.
#[allow(clippy::too_many_arguments)]
pub fn declare(
    store: &SessionWorkStore,
    session: &str,
    role: SessionWorkRole,
    request: &str,
    input: &DeclareInput,
    items: &[ObservedItem],
    defaults: &WaitTimeoutDefaults,
    now: DateTime<Utc>,
) -> Result<DeclareOutcome, SessionWorkError> {
    if input.action == "withdraw" && role == SessionWorkRole::Primary {
        return Ok(match store.withdraw(session, request, now)? {
            Some(record) => DeclareOutcome::Withdrawn(record),
            None => DeclareOutcome::Refused(invalid("you have no declaration to withdraw.")),
        });
    }
    let declaration = match build_declaration(input, role, defaults) {
        Ok(declaration) => declaration,
        Err(refusal) => return Ok(DeclareOutcome::Refused(refusal)),
    };
    if let Err(refusal) = validate(store, session, &declaration, items)? {
        return Ok(DeclareOutcome::Refused(refusal));
    }
    let (turn, _) = store.begin_turn(session, now)?;
    let (record, _) = store.declare(session, request, turn, &declaration, now)?;
    let also_running = match &record.declaration {
        Declaration::Wait { condition, .. } => {
            let named = condition.aliases();
            items
                .iter()
                .filter(|observed| observed.running && !named.contains(&observed.item.alias))
                .map(|observed| observed.item.clone())
                .collect()
        }
        Declaration::Complete { .. } => Vec::new(),
    };
    Ok(DeclareOutcome::Declared {
        record,
        also_running,
    })
}

/// What the gate says about one tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate {
    /// No declaration governs the turn, or the call is `declare_stop`.
    Run,
    /// The call runs and its result carries the warning.
    RunWarned,
    /// The call is refused. `end_turn` when the refused-call counter is used
    /// up; the turn then ends with the declaration intact.
    Refuse {
        wait: bool,
        batch: bool,
        end_turn: bool,
    },
}

/// Decide one tool call before it runs. `call` is the tool call's identity,
/// so a replayed decision is stable.
pub fn gate(
    store: &SessionWorkStore,
    session: &str,
    tool: &str,
    call: &str,
    limits: &CorrectionLimits,
) -> Result<Gate, SessionWorkError> {
    if tool == "declare_stop" {
        return Ok(Gate::Run);
    }
    let Some(current) = store.current_stop(session)? else {
        return Ok(Gate::Run);
    };
    let batch = tool == "batch";
    Ok(match store.gate_call(session, call, !batch)? {
        super::store::GateRecord::Warned => Gate::RunWarned,
        super::store::GateRecord::Refused { count } => Gate::Refuse {
            wait: current.declaration.is_wait(),
            batch,
            end_turn: count > limits.refused,
        },
    })
}

/// How a turn reached its natural end, as the turn loop saw it.
#[derive(Clone, Debug)]
pub struct NaturalEnd<'a> {
    /// The final message's visible text.
    pub final_text: &'a str,
    /// The identity of this end, such as the final assistant message ID.
    /// A correction counts once per occurrence.
    pub occurrence: &'a str,
    /// Set when the end was not the agent's choice: a provider refusal, or
    /// truncated output after continuations ran out.
    pub nonvoluntary: Option<TurnOutcome>,
    /// The final message had no visible output. Without a declaration this
    /// is treated as truncated output, never as a voluntary stop.
    pub empty: bool,
}

/// What the turn loop does at a natural end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndDecision {
    /// The turn ends with this outcome.
    End(TurnOutcome),
    /// Append this correction and continue the turn.
    Correct(Correction),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Correction {
    /// A stop without a declaration while the workflow is unfinished.
    Missing {
        unfinished: Vec<(String, ModuleStatus)>,
    },
    /// The current declaration no longer holds.
    Invalid(Refusal),
}

/// Decide a natural end of a turn: settle a declaration, allow implicit
/// completion, or correct the stop while its counter lasts.
pub fn natural_end(
    store: &SessionWorkStore,
    session: &str,
    end: &NaturalEnd<'_>,
    items: &[ObservedItem],
    limits: &CorrectionLimits,
    now: DateTime<Utc>,
) -> Result<EndDecision, SessionWorkError> {
    store.begin_turn(session, now)?;
    if let Some(outcome) = &end.nonvoluntary {
        // A refusal, truncation or empty response is not the agent's
        // decision to stop; a declaration made earlier stays as it is.
        return Ok(EndDecision::End(outcome.clone()));
    }
    if let Some(current) = store.current_stop(session)? {
        return match validate(store, session, &current.declaration, items)? {
            Ok(()) => Ok(EndDecision::End(settle_valid(
                store,
                session,
                current,
                Some(end.final_text),
                now,
            )?)),
            Err(refusal) => {
                store.settle_stop(
                    session,
                    current.number,
                    DeclarationState::Invalidated {
                        reason: refusal_summary(&refusal),
                    },
                    Some(end.final_text),
                    None,
                    now,
                )?;
                let count =
                    store.count_correction(session, ExhaustedCounter::Invalid, end.occurrence)?;
                Ok(if count > limits.invalid {
                    EndDecision::End(TurnOutcome::StoppedWithoutDeclaration {
                        cause: ExhaustedCounter::Invalid,
                    })
                } else {
                    EndDecision::Correct(Correction::Invalid(refusal))
                })
            }
        };
    }
    if end.empty {
        return Ok(EndDecision::End(TurnOutcome::Truncated));
    }
    let Some(unfinished) = undeclared_workflow(store, session)? else {
        return Ok(EndDecision::End(TurnOutcome::Completed { declared: false }));
    };
    let count = store.count_correction(session, ExhaustedCounter::Missing, end.occurrence)?;
    Ok(if count > limits.missing {
        EndDecision::End(TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Missing,
        })
    } else {
        EndDecision::Correct(Correction::Missing { unfinished })
    })
}

/// End a turn whose refused-call counter ran out. The declaration stays: it
/// becomes effective if it still holds.
pub fn end_after_refusals(
    store: &SessionWorkStore,
    session: &str,
    items: &[ObservedItem],
    now: DateTime<Utc>,
) -> Result<TurnOutcome, SessionWorkError> {
    let refused = TurnOutcome::StoppedWithoutDeclaration {
        cause: ExhaustedCounter::Refused,
    };
    let Some(current) = store.current_stop(session)? else {
        return Ok(refused);
    };
    match validate(store, session, &current.declaration, items)? {
        Ok(()) => settle_valid(store, session, current, None, now),
        Err(refusal) => {
            store.settle_stop(
                session,
                current.number,
                DeclarationState::Invalidated {
                    reason: refusal_summary(&refusal),
                },
                None,
                None,
                now,
            )?;
            Ok(refused)
        }
    }
}

/// Record how a turn ended. `request` identifies this recording.
pub fn record_outcome(
    store: &SessionWorkStore,
    session: &str,
    request: &str,
    outcome: &TurnOutcome,
    now: DateTime<Utc>,
) -> Result<(), SessionWorkError> {
    let (turn, _) = store.begin_turn(session, now)?;
    store.record_outcome(session, turn, request, outcome, now)
}

fn settle_valid(
    store: &SessionWorkStore,
    session: &str,
    current: StopRecord,
    final_text: Option<&str>,
    now: DateTime<Utc>,
) -> Result<TurnOutcome, SessionWorkError> {
    let revision = match current.declaration {
        Declaration::Complete { .. } => store
            .workflow_head(session)?
            .map(|head| head.revision.revision),
        Declaration::Wait { .. } => None,
    };
    let record = store.settle_stop(
        session,
        current.number,
        DeclarationState::Effective,
        final_text,
        revision,
        now,
    )?;
    Ok(if record.declaration.is_wait() {
        TurnOutcome::Waiting {
            stop: record.number,
        }
    } else {
        TurnOutcome::Completed { declared: true }
    })
}

/// The unfinished modules of a workflow that has not been declared complete
/// at its current revision, or `None` when implicit completion is allowed:
/// there is no workflow, or the newest completion covers its head.
pub fn undeclared_workflow(
    store: &SessionWorkStore,
    session: &str,
) -> Result<Option<Vec<(String, ModuleStatus)>>, SessionWorkError> {
    let Some(head) = store.workflow_head(session)? else {
        return Ok(None);
    };
    if store
        .latest_completion(session)?
        .is_some_and(|completion| completion.workflow_revision == Some(head.revision.revision))
    {
        return Ok(None);
    }
    let workflow = super::parse_workflow(&head.revision.text).map_err(|errors| {
        SessionWorkError::Corrupt(format!(
            "stored workflow revision {} no longer parses: {errors}",
            head.revision.revision
        ))
    })?;
    Ok(Some(
        workflow
            .unfinished()
            .into_iter()
            .map(|module| (module.id.as_str().to_string(), module.status))
            .collect(),
    ))
}

/// A short record of why a declaration was refused, kept with it.
pub fn refusal_summary(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Unfinished(modules) => format!("unfinished modules: {}", module_list(modules)),
        Refusal::StillRunning(items) => format!("still running: {}", item_list(items)),
        Refusal::Invalid(reason) => reason.trim_end_matches('.').to_string(),
    }
}

/// `proto (active), after (undetermined)`.
pub fn module_list(modules: &[(String, ModuleStatus)]) -> String {
    modules
        .iter()
        .map(|(id, status)| format!("{id} ({status})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `t2 (lint, wake), c1 (research, hold)`.
pub fn item_list(items: &[SessionItem]) -> String {
    items
        .iter()
        .map(|item| {
            let policy = match item.policy {
                FinishPolicy::Wake => "wake",
                FinishPolicy::Hold => "hold",
            };
            format!("{} ({}, {policy})", item.alias, item.label)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `t2 (lint, wake) and c1 (research, hold)`.
pub fn item_list_and(items: &[SessionItem]) -> String {
    let rendered: Vec<String> = items
        .iter()
        .map(|item| item_list(std::slice::from_ref(item)))
        .collect();
    join_and(&rendered)
}

fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
