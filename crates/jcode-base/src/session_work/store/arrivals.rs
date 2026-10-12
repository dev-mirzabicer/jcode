//! Arrival records: each input's route, decided once in the same transaction
//! that applies its effect on the stop, and the release of held arrivals.
use super::stops::{current_stop, open_turn, stops, write_stop};
use super::*;
use crate::session_work::arrivals::{self as rule, Activity, SessionState};
use jcode_session_work_types::{
    ArrivalClass, ArrivalRecord, ArrivalRoute, DeclarationState, HumanDisposition, TurnOutcome,
    WithdrawCause,
};
use std::collections::HashSet;

impl SessionWorkStore {
    /// Decide an input's arrival and record it, with its effect on the stop,
    /// in one transaction. An input already recorded returns its original
    /// record, so a retried or recovered delivery converges.
    pub fn arrive(
        &self,
        session: &str,
        input: &str,
        class: &ArrivalClass,
        activity: Activity,
        now: DateTime<Utc>,
    ) -> Result<ArrivalRecord, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        if let Some(existing) = read_arrival(&transaction, session, input)? {
            return Ok(existing);
        }
        let item = match class.item() {
            Some(source) => Some(finish_item(&transaction, session, source, now)?),
            None => None,
        };
        let state = session_state(&transaction, session, activity)?;
        let finished = finished_aliases(&transaction, session)?;
        let route = rule::decide(&state, class, item.as_ref(), &|alias| {
            finished.contains(&alias)
        });
        let cause = match route {
            ArrivalRoute::EndsStop { stop } => Some((stop, WithdrawCause::AwaitedArrival)),
            ArrivalRoute::RevokesStop { stop } => Some((stop, WithdrawCause::Revoked)),
            ArrivalRoute::TimesOut { stop } => Some((stop, WithdrawCause::Timeout)),
            _ => None,
        };
        if let Some((stop, cause)) = cause {
            withdraw(&transaction, session, stop, cause, now)?;
        }
        let record = ArrivalRecord {
            input: input.to_string(),
            class: class.clone(),
            arrived_at: now,
            route,
            item,
            released_at: None,
        };
        let sequence: i64 = transaction
            .query_row(
                "SELECT coalesce(max(sequence), 0) + 1 FROM arrivals WHERE session=?1",
                [session],
                |row| row.get(0),
            )
            .map_err(SessionWorkError::corrupt)?;
        transaction
            .execute(
                "INSERT INTO arrivals(session, input, sequence, body) VALUES(?1, ?2, ?3, ?4)",
                params![session, input, sequence, encode(&record)?],
            )
            .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(record)
    }

    /// The recorded arrival of one input.
    pub fn arrival(
        &self,
        session: &str,
        input: &str,
    ) -> Result<Option<ArrivalRecord>, SessionWorkError> {
        let connection = self.open()?;
        read_arrival(&connection, session, input)
    }

    /// Every recorded arrival of a session, oldest first.
    pub fn arrivals(&self, session: &str) -> Result<Vec<ArrivalRecord>, SessionWorkError> {
        let connection = self.open()?;
        all_arrivals(&connection, session)
    }

    /// Release the held arrivals the rule now delivers, in arrival order.
    /// A turn that starts (`Working`) releases everything the session's
    /// state no longer holds: all of it, unless a stop is still declared.
    pub fn release_held(
        &self,
        session: &str,
        activity: Activity,
        now: DateTime<Utc>,
    ) -> Result<Vec<ArrivalRecord>, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let held: Vec<ArrivalRecord> = all_arrivals(&transaction, session)?
            .into_iter()
            .filter(ArrivalRecord::is_held)
            .collect();
        if held.is_empty() {
            return Ok(Vec::new());
        }
        let state = session_state(&transaction, session, activity)?;
        let finished = finished_aliases(&transaction, session)?;
        let mut released = Vec::new();
        for mut record in held {
            let now_route = rule::decide(&state, &record.class, record.item.as_ref(), &|alias| {
                finished.contains(&alias)
            });
            if matches!(
                now_route,
                ArrivalRoute::Held { .. } | ArrivalRoute::Obsolete
            ) {
                continue;
            }
            record.released_at = Some(now);
            write_arrival(&transaction, session, &record)?;
            released.push(record);
        }
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(released)
    }

    /// Mirza chose how to send a message the rule held. With a stop still
    /// declared, send-and-keep delivers it under the stop and
    /// revoke-and-send withdraws the stop; without one, it is released.
    /// Queue leaves it held. A message already delivered is unchanged.
    pub fn redispose(
        &self,
        session: &str,
        input: &str,
        disposition: HumanDisposition,
        now: DateTime<Utc>,
    ) -> Result<ArrivalRecord, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let mut record = read_arrival(&transaction, session, input)?.ok_or_else(|| {
            SessionWorkError::Invalid(format!("input {input} has not arrived in {session}"))
        })?;
        if !matches!(record.class, ArrivalClass::Human { .. }) {
            return Err(SessionWorkError::Invalid(format!(
                "input {input} is not a message from Mirza"
            )));
        }
        if !record.is_held() || disposition == HumanDisposition::Queue {
            return Ok(record);
        }
        record.class = ArrivalClass::Human { disposition };
        match current_stop(&transaction, session)? {
            Some(stop) => {
                record.route = match disposition {
                    HumanDisposition::SendKeepStop => ArrivalRoute::KeepsStop { stop: stop.number },
                    _ => {
                        withdraw(
                            &transaction,
                            session,
                            stop.number,
                            WithdrawCause::Revoked,
                            now,
                        )?;
                        ArrivalRoute::RevokesStop { stop: stop.number }
                    }
                };
            }
            None => record.released_at = Some(now),
        }
        write_arrival(&transaction, session, &record)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(record)
    }

    /// The row of the arrival table the session is in now.
    pub fn session_state(
        &self,
        session: &str,
        activity: Activity,
    ) -> Result<SessionState, SessionWorkError> {
        let connection = self.open()?;
        require_activation(&connection, session)?;
        session_state(&connection, session, activity)
    }
}

/// Mark the arrival's item finished, allocating it if it was never seen.
fn finish_item(
    connection: &Connection,
    session: &str,
    source: &jcode_session_work_types::ItemSource,
    now: DateTime<Utc>,
) -> Result<SessionItem, SessionWorkError> {
    let mut item = match item_by_request(connection, session, &source.request)? {
        Some(item) => item,
        None => allocate_in(
            connection,
            session,
            &source.request,
            source.kind,
            &source.label,
            source.policy,
            now,
        )?,
    };
    if item.finished_at.is_none() {
        item.finished_at = Some(now);
        connection
            .execute(
                "UPDATE items SET body=?3 WHERE session=?1 AND request=?2",
                params![session, source.request, encode(&item)?],
            )
            .map_err(SessionWorkError::io)?;
    }
    Ok(item)
}

pub(super) fn finished_aliases(
    connection: &Connection,
    session: &str,
) -> Result<HashSet<ItemAlias>, SessionWorkError> {
    let mut statement = connection
        .prepare("SELECT body FROM items WHERE session=?1")
        .map_err(SessionWorkError::corrupt)?;
    let rows = statement
        .query_map([session], |row| row.get::<_, String>(0))
        .map_err(SessionWorkError::corrupt)?;
    let mut finished = HashSet::new();
    for row in rows {
        let item: SessionItem = decode(&row.map_err(SessionWorkError::corrupt)?)?;
        if item.finished_at.is_some() {
            finished.insert(item.alias);
        }
    }
    Ok(finished)
}

fn session_state(
    connection: &Connection,
    session: &str,
    activity: Activity,
) -> Result<SessionState, SessionWorkError> {
    if let Some(stop) = current_stop(connection, session)? {
        return Ok(SessionState::Declared(stop));
    }
    if activity == Activity::Working {
        return Ok(SessionState::Working);
    }
    // An open turn while idle never finished: a crash, or a reload whose
    // continuation has not run yet.
    if open_turn(connection, session)?.is_some() {
        return Ok(SessionState::Interrupted);
    }
    let last: Option<String> = connection
        .query_row(
            "SELECT body FROM turn_outcomes WHERE session=?1 ORDER BY sequence DESC LIMIT 1",
            [session],
            |row| row.get(0),
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?;
    let Some(last) = last else {
        return Ok(SessionState::Completed);
    };
    Ok(match decode::<TurnOutcome>(&last)? {
        // A wait that no longer governs was ended by an arrival, which
        // starts the session's next turn.
        TurnOutcome::Completed { .. } | TurnOutcome::Waiting { .. } => SessionState::Completed,
        TurnOutcome::InterruptedByUser
        | TurnOutcome::InterruptedByReload
        | TurnOutcome::ProviderRefusal
        | TurnOutcome::Truncated
        | TurnOutcome::Failed { .. }
        | TurnOutcome::StoppedWithoutDeclaration { .. } => SessionState::Interrupted,
    })
}

/// Withdraw stop `number` if it still governs the session.
fn withdraw(
    connection: &Connection,
    session: &str,
    number: u32,
    cause: WithdrawCause,
    now: DateTime<Utc>,
) -> Result<(), SessionWorkError> {
    let Some(mut stop) = stops(connection, session)?
        .into_iter()
        .find(|stop| stop.number == number && stop.is_current())
    else {
        return Ok(());
    };
    stop.state = DeclarationState::Withdrawn { cause };
    stop.settled_at.get_or_insert(now);
    write_stop(connection, session, &stop, None)
}

fn read_arrival(
    connection: &Connection,
    session: &str,
    input: &str,
) -> Result<Option<ArrivalRecord>, SessionWorkError> {
    connection
        .query_row(
            "SELECT body FROM arrivals WHERE session=?1 AND input=?2",
            params![session, input],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?
        .map(|body| decode(&body))
        .transpose()
}

fn all_arrivals(
    connection: &Connection,
    session: &str,
) -> Result<Vec<ArrivalRecord>, SessionWorkError> {
    let mut statement = connection
        .prepare("SELECT body FROM arrivals WHERE session=?1 ORDER BY sequence")
        .map_err(SessionWorkError::corrupt)?;
    let rows = statement
        .query_map([session], |row| row.get::<_, String>(0))
        .map_err(SessionWorkError::corrupt)?;
    rows.map(|row| decode(&row.map_err(SessionWorkError::corrupt)?))
        .collect()
}

fn write_arrival(
    connection: &Connection,
    session: &str,
    record: &ArrivalRecord,
) -> Result<(), SessionWorkError> {
    let changed = connection
        .execute(
            "UPDATE arrivals SET body=?3 WHERE session=?1 AND input=?2",
            params![session, record.input, encode(record)?],
        )
        .map_err(SessionWorkError::io)?;
    if changed != 1 {
        return Err(SessionWorkError::Corrupt(format!(
            "session {session} has no arrival {}",
            record.input
        )));
    }
    Ok(())
}
