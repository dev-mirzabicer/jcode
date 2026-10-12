//! Durable primary input through the arrival rule. The C01 inbox keeps each
//! accepted input until it is committed; this module records each input's
//! arrival once, marks held input on its receipt, releases held input when
//! the session's state delivers it, and orders what may be delivered now:
//! released held input first, oldest first, then the rest in inbox order.
use super::arrivals::{self, Activity, Lead};
use super::{SessionWorkError, SessionWorkStore};
use crate::instruction::notification::Notification;
use crate::primary_input::{PrimaryInputDelivery, PrimaryInputEnvelope, PrimaryInputStore};
use anyhow::Result;
use chrono::{DateTime, Utc};
use jcode_session_work_types::{ArrivalRecord, ArrivalRoute, HeldWhile};
use std::collections::HashMap;
use std::path::Path;

/// One input that may be delivered now, with the note that comes before it.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub input: PrimaryInputEnvelope,
    pub lead: Option<Lead>,
    /// It was held and is now released.
    pub released: bool,
    /// Its place in the session's arrival order, once recorded.
    pub position: Option<usize>,
}

/// Whether an input's arrival is decided now. Context-only input never
/// starts a turn and is outside the rule. Next-turn input waits for the turn
/// to end, so its arrival is decided against the session's state then, not
/// while the turn that may still declare a stop is running.
pub fn decided_now(input: &PrimaryInputEnvelope, activity: Activity) -> bool {
    match input.delivery {
        PrimaryInputDelivery::ContextOnly => false,
        PrimaryInputDelivery::NextTurn => activity == Activity::Idle,
        PrimaryInputDelivery::SafeBoundary => true,
    }
}

/// Record an accepted input's arrival. A held input's receipt carries the
/// held diagnostic until it is released.
pub fn arrive(
    session: &str,
    input: &PrimaryInputEnvelope,
    activity: Activity,
) -> Result<ArrivalRecord, SessionWorkError> {
    let record = SessionWorkStore::new().arrive(
        session,
        &input.id.to_string(),
        &input.arrival_class(),
        activity,
        Utc::now(),
    )?;
    if let ArrivalRoute::Held { held_while } = &record.route
        && record.released_at.is_none()
        && let Err(error) = PrimaryInputStore::current().defer(
            session,
            input.id,
            Some(arrivals::held_diagnostic(held_while)),
        )
    {
        crate::logging::warn(&format!(
            "Session work: held input {} of {session} has no visible diagnostic: {error:#}",
            input.id
        ));
    }
    Ok(record)
}

/// The pending inputs that may be delivered now. Inputs not yet recorded
/// arrive first; held inputs the session's state now delivers are released;
/// an obsolete input is settled so it never waits forever.
pub fn route_pending(
    session: &str,
    pending: Vec<PrimaryInputEnvelope>,
    activity: Activity,
) -> Result<Vec<Delivery>> {
    let store = SessionWorkStore::new();
    let inbox = PrimaryInputStore::current();
    for input in &pending {
        if decided_now(input, activity) && store.arrival(session, &input.id.to_string())?.is_none()
        {
            arrive(session, input, activity)?;
        }
    }
    for record in store.release_held(session, activity, Utc::now())? {
        if let Ok(id) = record.input.parse()
            && let Err(error) = inbox.defer(session, id, None)
        {
            crate::logging::warn(&format!(
                "Session work: released input {} of {session} keeps its held diagnostic: {error:#}",
                record.input
            ));
        }
    }
    let records = store.arrivals(session)?;
    let order: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.input.as_str(), index))
        .collect();
    let by_input: HashMap<&str, &ArrivalRecord> = records
        .iter()
        .map(|record| (record.input.as_str(), record))
        .collect();
    let mut released = Vec::new();
    let mut rest = Vec::new();
    for input in pending {
        let id = input.id.to_string();
        let Some(record) = by_input.get(id.as_str()) else {
            // Outside the rule now: context-only input is delivered as it
            // is; next-turn input waits for the turn to end.
            if input.delivery == PrimaryInputDelivery::ContextOnly {
                rest.push(Delivery {
                    input,
                    lead: None,
                    released: false,
                    position: None,
                });
            }
            continue;
        };
        if record.route == ArrivalRoute::Obsolete {
            settle_obsolete(session, &input)?;
            continue;
        }
        if !arrivals::deliverable(record) {
            continue;
        }
        let delivery = Delivery {
            lead: arrivals::lead(record),
            released: matches!(record.route, ArrivalRoute::Held { .. }),
            position: order.get(id.as_str()).copied(),
            input,
        };
        if delivery.released {
            released.push(delivery);
        } else {
            rest.push(delivery);
        }
    }
    released.sort_by_key(|delivery| delivery.position);
    Ok(released.into_iter().chain(rest).collect())
}

/// Whether any pending input may be delivered now, without changing state.
/// An input not yet recorded counts: its arrival is decided when it is
/// routed.
pub fn any_deliverable(session: &str, pending: &[PrimaryInputEnvelope]) -> Result<bool> {
    let store = SessionWorkStore::new();
    for input in pending {
        match store.arrival(session, &input.id.to_string())? {
            None => return Ok(true),
            Some(record) if arrivals::deliverable(&record) => return Ok(true),
            Some(_) => {}
        }
    }
    Ok(false)
}

/// The recorded arrival of one input, if the session has session work.
pub fn recorded(session: &str, input: &PrimaryInputEnvelope) -> Result<Option<ArrivalRecord>> {
    Ok(SessionWorkStore::new().arrival(session, &input.id.to_string())?)
}

fn settle_obsolete(session: &str, input: &PrimaryInputEnvelope) -> Result<()> {
    let stub = crate::session::Session::load_startup_stub(session)?;
    let mut lease = PrimaryInputStore::current().lock(session)?;
    lease.cancel(&stub, input.id)?;
    Ok(())
}

/// The approved text of a lead, rendered from its managed resource.
pub fn render_lead(lead: &Lead, working_dir: Option<&Path>) -> Result<String> {
    let rendered = match lead {
        Lead::HeldItem {
            alias,
            label,
            event,
            at,
            held_while,
        } => {
            let alias = alias.to_string();
            let time = clock(*at);
            match held_while {
                HeldWhile::Waiting { condition } => Notification::SessionWorkHeldItemWaiting {
                    alias: &alias,
                    label,
                    event: event.text(),
                    time: &time,
                    condition: &condition.to_string(),
                }
                .render(working_dir)?,
                HeldWhile::Stopped => Notification::SessionWorkHeldItemStopped {
                    alias: &alias,
                    label,
                    event: event.text(),
                    time: &time,
                }
                .render(working_dir)?,
            }
        }
        Lead::HeldMessage { at, held_while } => {
            let time = clock(*at);
            match held_while {
                HeldWhile::Waiting { condition } => Notification::SessionWorkHeldMessageWaiting {
                    time: &time,
                    condition: &condition.to_string(),
                }
                .render(working_dir)?,
                HeldWhile::Stopped => Notification::SessionWorkHeldMessageStopped { time: &time }
                    .render(working_dir)?,
            }
        }
        Lead::HeldNotice { at, held_while } => {
            let time = clock(*at);
            match held_while {
                HeldWhile::Waiting { condition } => Notification::SessionWorkHeldNoticeWaiting {
                    time: &time,
                    condition: &condition.to_string(),
                }
                .render(working_dir)?,
                HeldWhile::Stopped => Notification::SessionWorkHeldNoticeStopped { time: &time }
                    .render(working_dir)?,
            }
        }
        Lead::StopEnded { alias, event } => Notification::SessionWorkStopEnded {
            alias: &alias.to_string(),
            event: event.text(),
        }
        .render(working_dir)?,
        Lead::KeptStop => Notification::SessionWorkMessageKeptStop.render(working_dir)?,
        Lead::RevokedStop => Notification::SessionWorkMessageRevokedStop.render(working_dir)?,
    };
    anyhow::ensure!(
        !rendered.trim().is_empty(),
        "A session-work arrival note rendered empty; repair its managed source"
    );
    Ok(rendered)
}

/// Local wall-clock time, with the date when it is not today.
fn clock(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&chrono::Local);
    if local.date_naive() == chrono::Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}
