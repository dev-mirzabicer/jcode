//! The arrival rule: one decision for everything that reaches a session from
//! outside its own turn. Mirza's messages, finished background work,
//! answers, timers and scheduled resumes are each delivered at the next safe
//! point (or start a turn), held until the session wakes, or end the stop
//! they were awaited by. The store records each decision once, together with
//! its effect on the stop, so a retried or recovered delivery converges.
//!
//! The table this implements is in `docs/SESSION_WORK.md` ("Arrivals").
use chrono::{DateTime, Utc};
use jcode_session_work_types::{
    ArrivalClass, ArrivalRecord, ArrivalRoute, Declaration, FinishPolicy, HeldWhile,
    HumanDisposition, ItemAlias, ItemKind, SessionItem, StopRecord,
};

/// Whether the session's turn is running when an arrival is decided.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activity {
    /// A turn is running: the arrival is considered at its next safe point.
    Working,
    /// No turn is running: a delivered arrival starts one.
    Idle,
}

/// The rows of the arrival table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// A turn is running and no stop is declared.
    Working,
    /// A stop is declared: pending in the turn that declares it, or an
    /// effective wait. This row wins over every other.
    Declared(StopRecord),
    /// The last turn completed, implicitly or by declaration, or the session
    /// has not had a turn yet.
    Completed,
    /// The last turn was stopped by Mirza, failed, was refused or
    /// truncated, ran out of corrections, or never finished (a crash or a
    /// reload not yet continued).
    Interrupted,
}

/// The arrival rule. `item` is the arrival's own item, already marked
/// finished; `finished` answers for every other alias of the session.
pub fn decide(
    state: &SessionState,
    class: &ArrivalClass,
    item: Option<&SessionItem>,
    finished: &dyn Fn(ItemAlias) -> bool,
) -> ArrivalRoute {
    let policy = item
        .map(|item| item.policy)
        .or_else(|| class.item().map(|source| source.policy))
        .unwrap_or_default();
    match state {
        SessionState::Working => match class {
            ArrivalClass::TimerExpired { .. } => ArrivalRoute::Obsolete,
            _ => ArrivalRoute::Deliver,
        },
        SessionState::Declared(stop) => decide_declared(stop, class, item, finished),
        SessionState::Completed => match class {
            ArrivalClass::TimerExpired { .. } => ArrivalRoute::Obsolete,
            ArrivalClass::Answer { .. } | ArrivalClass::ItemFinished { .. }
                if policy == FinishPolicy::Hold =>
            {
                held(HeldWhile::Stopped)
            }
            _ => ArrivalRoute::Deliver,
        },
        SessionState::Interrupted => match class {
            ArrivalClass::TimerExpired { .. } => ArrivalRoute::Obsolete,
            ArrivalClass::ItemFinished { .. } | ArrivalClass::SystemNotice => {
                held(HeldWhile::Stopped)
            }
            ArrivalClass::Answer { .. } if policy == FinishPolicy::Hold => held(HeldWhile::Stopped),
            _ => ArrivalRoute::Deliver,
        },
    }
}

fn held(held_while: HeldWhile) -> ArrivalRoute {
    ArrivalRoute::Held { held_while }
}

fn decide_declared(
    stop: &StopRecord,
    class: &ArrivalClass,
    item: Option<&SessionItem>,
    finished: &dyn Fn(ItemAlias) -> bool,
) -> ArrivalRoute {
    let condition = match &stop.declaration {
        Declaration::Wait { condition, .. } => Some(condition),
        Declaration::Complete { .. } => None,
    };
    let held_while = match condition {
        Some(condition) => HeldWhile::Waiting {
            condition: condition.clone(),
        },
        None => HeldWhile::Stopped,
    };
    match class {
        ArrivalClass::Human { disposition } => match disposition {
            HumanDisposition::Queue => held(held_while),
            HumanDisposition::SendKeepStop => ArrivalRoute::KeepsStop { stop: stop.number },
            HumanDisposition::RevokeAndSend => ArrivalRoute::RevokesStop { stop: stop.number },
        },
        ArrivalClass::TimerExpired { stop: number } => {
            if *number == stop.number && condition.is_some() {
                ArrivalRoute::TimesOut { stop: stop.number }
            } else {
                ArrivalRoute::Obsolete
            }
        }
        ArrivalClass::Continuation => ArrivalRoute::Deliver,
        ArrivalClass::SystemNotice => held(held_while),
        ArrivalClass::ItemFinished { .. }
        | ArrivalClass::Answer { .. }
        | ArrivalClass::ScheduledResume { .. } => {
            if let (Some(condition), Some(item)) = (condition, item)
                && condition.aliases().contains(&item.alias)
            {
                return if condition.is_met(&|alias| alias == item.alias || finished(alias)) {
                    ArrivalRoute::EndsStop { stop: stop.number }
                } else {
                    held(held_while)
                };
            }
            match class {
                // A scheduled resume wakes the session; the stop stays.
                ArrivalClass::ScheduledResume { .. } => ArrivalRoute::Deliver,
                _ => held(held_while),
            }
        }
    }
}

/// Whether an arrival recorded with this route may be delivered now. A held
/// arrival is delivered once released.
pub fn deliverable(record: &ArrivalRecord) -> bool {
    match record.route {
        ArrivalRoute::Held { .. } => record.released_at.is_some(),
        ArrivalRoute::Obsolete => false,
        _ => true,
    }
}

/// The code-written note that comes before an arrival when it is delivered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lead {
    /// A held item: `[Held] t2 (lint) finished at 14:02 while you were ...`.
    HeldItem {
        alias: ItemAlias,
        label: String,
        event: ItemEvent,
        at: DateTime<Utc>,
        held_while: HeldWhile,
    },
    /// A message from Mirza that was held.
    HeldMessage {
        at: DateTime<Utc>,
        held_while: HeldWhile,
    },
    /// Any other held notice.
    HeldNotice {
        at: DateTime<Utc>,
        held_while: HeldWhile,
    },
    /// An awaited item ended the stop.
    StopEnded { alias: ItemAlias, event: ItemEvent },
    /// Mirza sent a message during a stop and kept the stop.
    KeptStop,
    /// Mirza sent a message during a stop and withdrew the stop.
    RevokedStop,
}

/// How an item reached its final state, in the approved wording's verbs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemEvent {
    Finished,
    WasAnswered,
    Fired,
}

impl ItemEvent {
    pub fn of(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Task | ItemKind::Child | ItemKind::ProposalDone => Self::Finished,
            ItemKind::Question | ItemKind::Artifact | ItemKind::Proposal => Self::WasAnswered,
            ItemKind::Resume => Self::Fired,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::WasAnswered => "was answered",
            Self::Fired => "fired",
        }
    }
}

/// The note to deliver before this arrival, if it needs one.
pub fn lead(record: &ArrivalRecord) -> Option<Lead> {
    match &record.route {
        ArrivalRoute::Held { held_while } => {
            let held_while = held_while.clone();
            let at = record.arrived_at;
            Some(match (&record.class, &record.item) {
                (ArrivalClass::Human { .. }, _) => Lead::HeldMessage { at, held_while },
                (_, Some(item)) => Lead::HeldItem {
                    alias: item.alias,
                    label: item.label.clone(),
                    event: ItemEvent::of(item.alias.kind()),
                    at,
                    held_while,
                },
                _ => Lead::HeldNotice { at, held_while },
            })
        }
        ArrivalRoute::EndsStop { .. } => record.item.as_ref().map(|item| Lead::StopEnded {
            alias: item.alias,
            event: ItemEvent::of(item.alias.kind()),
        }),
        ArrivalRoute::KeepsStop { .. } => Some(Lead::KeptStop),
        ArrivalRoute::RevokesStop { .. } => Some(Lead::RevokedStop),
        // The timer notices belong to the timers; deliver as is.
        ArrivalRoute::TimesOut { .. } | ArrivalRoute::Deliver | ArrivalRoute::Obsolete => None,
    }
}

/// The receipt diagnostic of a held input, shown by clients while it waits.
pub fn held_diagnostic(held_while: &HeldWhile) -> String {
    match held_while {
        HeldWhile::Waiting { condition } => {
            format!("Held: the session is waiting on {condition}.")
        }
        HeldWhile::Stopped => "Held until the session's next turn.".to_string(),
    }
}

#[cfg(test)]
#[path = "arrivals_tests.rs"]
mod tests;
