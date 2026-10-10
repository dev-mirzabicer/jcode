//! Stop declarations, per-turn counters and turn outcomes. Every mutation is
//! idempotent by its request: an invocation identity, a tool call or an
//! occurrence.
use super::*;
use jcode_session_work_types::{
    Declaration, DeclarationState, ExhaustedCounter, StopRecord, TurnOutcome, WithdrawCause,
};
use serde::{Deserialize, Serialize};

/// One turn's counters. A turn stays open until it reaches an outcome that
/// settles it; a reload or crash is continued by the session's next turn,
/// which keeps these counters.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnCounters {
    pub started_at: Option<DateTime<Utc>>,
    /// The tool call that ran with the warning under the current declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warned_call: Option<String>,
    /// Tool calls refused under a declaration, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_calls: Vec<String>,
    /// Corrections given, by counter and occurrence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corrections: Vec<(ExhaustedCounter, String)>,
}

impl TurnCounters {
    pub fn corrections(&self, counter: ExhaustedCounter) -> u32 {
        self.corrections
            .iter()
            .filter(|(kind, _)| *kind == counter)
            .count() as u32
    }
}

/// What the gate recorded for one tool call under a declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateRecord {
    /// The call runs, with the warning.
    Warned,
    /// The call is refused; `count` refusals this turn, this one included.
    Refused { count: u32 },
}

impl SessionWorkStore {
    /// The session's open turn, opening the next one when the last has
    /// settled. Returns its number and counters.
    pub fn begin_turn(
        &self,
        session: &str,
        now: DateTime<Utc>,
    ) -> Result<(u32, TurnCounters), SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        if let Some(open) = open_turn(&transaction, session)? {
            return Ok(open);
        }
        let turn: u32 = transaction
            .query_row(
                "SELECT coalesce(max(turn), 0) + 1 FROM turn_counters WHERE session=?1",
                [session],
                |row| row.get(0),
            )
            .map_err(SessionWorkError::corrupt)?;
        let counters = TurnCounters {
            started_at: Some(now),
            ..TurnCounters::default()
        };
        transaction
            .execute(
                "INSERT INTO turn_counters(session, turn, body) VALUES(?1, ?2, ?3)",
                params![session, turn, encode(&counters)?],
            )
            .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok((turn, counters))
    }

    /// The open turn, if any.
    pub fn open_turn(
        &self,
        session: &str,
    ) -> Result<Option<(u32, TurnCounters)>, SessionWorkError> {
        let connection = self.open()?;
        open_turn(&connection, session)
    }

    /// Record a turn outcome once per request. An outcome that settles the
    /// turn closes it.
    pub fn record_outcome(
        &self,
        session: &str,
        turn: u32,
        request: &str,
        outcome: &TurnOutcome,
        now: DateTime<Utc>,
    ) -> Result<(), SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM turn_outcomes WHERE session=?1 AND request=?2",
                params![session, request],
                |row| row.get(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some(existing) = existing {
            let existing: TurnOutcome = decode(&existing)?;
            if existing != *outcome {
                return Err(SessionWorkError::Conflict(format!(
                    "request {request} already recorded a different turn outcome"
                )));
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO turn_outcomes(session, turn, request, at, body)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![session, turn, request, now.to_rfc3339(), encode(outcome)?],
            )
            .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)
    }

    /// Every recorded outcome of a session, oldest first: `(turn, outcome)`.
    pub fn turn_outcomes(
        &self,
        session: &str,
    ) -> Result<Vec<(u32, TurnOutcome)>, SessionWorkError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare("SELECT turn, body FROM turn_outcomes WHERE session=?1 ORDER BY sequence")
            .map_err(SessionWorkError::corrupt)?;
        let rows = statement
            .query_map([session], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(SessionWorkError::corrupt)?;
        rows.map(|row| {
            let (turn, body) = row.map_err(SessionWorkError::corrupt)?;
            Ok((turn, decode(&body)?))
        })
        .collect()
    }

    /// Record a pending declaration. A current declaration is replaced. The
    /// same request returns its original record; a different declaration
    /// under the same request is a conflict.
    pub fn declare(
        &self,
        session: &str,
        request: &str,
        turn: u32,
        declaration: &Declaration,
        now: DateTime<Utc>,
    ) -> Result<(StopRecord, Option<u32>), SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM stops WHERE session=?1 AND request=?2",
                params![session, request],
                |row| row.get(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some(existing) = existing {
            let existing: StopRecord = decode(&existing)?;
            if existing.declaration != *declaration {
                return Err(SessionWorkError::Conflict(format!(
                    "request {request} already declared stop {} differently",
                    existing.number
                )));
            }
            return Ok((existing, None));
        }
        let replaced = match current_stop(&transaction, session)? {
            Some(mut current) => {
                current.state = DeclarationState::Withdrawn {
                    cause: WithdrawCause::Replaced,
                };
                current.settled_at = Some(now);
                write_stop(&transaction, session, &current, Some(request))?;
                Some(current.number)
            }
            None => None,
        };
        let number: u32 = transaction
            .query_row(
                "SELECT coalesce(max(number), 0) + 1 FROM stops WHERE session=?1",
                [session],
                |row| row.get(0),
            )
            .map_err(SessionWorkError::corrupt)?;
        let record = StopRecord {
            number,
            turn,
            declaration: declaration.clone(),
            state: DeclarationState::Pending,
            declared_at: now,
            settled_at: None,
            final_text: None,
            workflow_revision: None,
        };
        transaction
            .execute(
                "INSERT INTO stops(session, number, request, body) VALUES(?1, ?2, ?3, ?4)",
                params![session, number, request, encode(&record)?],
            )
            .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok((record, replaced))
    }

    /// Withdraw the current declaration at the agent's request and give the
    /// turn's warned call back. Returns the withdrawn record, or `None` when
    /// nothing was declared. A repeated request returns its original record.
    pub fn withdraw(
        &self,
        session: &str,
        request: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<StopRecord>, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let replay: Option<String> = transaction
            .query_row(
                "SELECT body FROM stops WHERE session=?1 AND settled_request=?2",
                params![session, request],
                |row| row.get(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some(replay) = replay {
            return decode(&replay).map(Some);
        }
        let Some(mut current) = current_stop(&transaction, session)? else {
            return Ok(None);
        };
        current.state = DeclarationState::Withdrawn {
            cause: WithdrawCause::Agent,
        };
        current.settled_at = Some(now);
        write_stop(&transaction, session, &current, Some(request))?;
        if let Some((turn, mut counters)) = open_turn(&transaction, session)? {
            counters.warned_call = None;
            write_counters(&transaction, session, turn, &counters)?;
        }
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(Some(current))
    }

    /// Move a declaration to its end-of-turn state. Only a current
    /// declaration changes; a settled one is returned unchanged.
    pub fn settle_stop(
        &self,
        session: &str,
        number: u32,
        state: DeclarationState,
        final_text: Option<&str>,
        workflow_revision: Option<u32>,
        now: DateTime<Utc>,
    ) -> Result<StopRecord, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        let mut record = read_stop(&transaction, session, number)?.ok_or_else(|| {
            SessionWorkError::Invalid(format!("session {session} has no stop {number}"))
        })?;
        if !record.is_current() {
            return Ok(record);
        }
        let was_pending = record.state == DeclarationState::Pending;
        record.state = state;
        if was_pending {
            record.final_text = final_text.map(str::to_string);
            record.settled_at = Some(now);
        }
        if workflow_revision.is_some() {
            record.workflow_revision = workflow_revision;
        }
        write_stop(&transaction, session, &record, None)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(record)
    }

    /// The declaration that governs the session now, if any.
    pub fn current_stop(&self, session: &str) -> Result<Option<StopRecord>, SessionWorkError> {
        let connection = self.open()?;
        current_stop(&connection, session)
    }

    /// The newest completion that became effective.
    pub fn latest_completion(&self, session: &str) -> Result<Option<StopRecord>, SessionWorkError> {
        let connection = self.open()?;
        Ok(stops(&connection, session)?
            .into_iter()
            .rev()
            .find(|record| {
                record.state == DeclarationState::Effective && !record.declaration.is_wait()
            }))
    }

    /// Every declaration of a session, oldest first.
    pub fn stops(&self, session: &str) -> Result<Vec<StopRecord>, SessionWorkError> {
        let connection = self.open()?;
        stops(&connection, session)
    }

    /// Record the gate's decision for one tool call under a declaration. A
    /// call already recorded keeps its decision.
    pub fn gate_call(
        &self,
        session: &str,
        call: &str,
        allow_warned: bool,
    ) -> Result<GateRecord, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        let (turn, mut counters) = open_turn(&transaction, session)?.ok_or_else(|| {
            SessionWorkError::Invalid(format!("session {session} has no open turn"))
        })?;
        if counters.warned_call.as_deref() == Some(call) {
            return Ok(GateRecord::Warned);
        }
        if let Some(index) = counters.refused_calls.iter().position(|id| id == call) {
            return Ok(GateRecord::Refused {
                count: index as u32 + 1,
            });
        }
        let record = if allow_warned && counters.warned_call.is_none() {
            counters.warned_call = Some(call.to_string());
            GateRecord::Warned
        } else {
            counters.refused_calls.push(call.to_string());
            GateRecord::Refused {
                count: counters.refused_calls.len() as u32,
            }
        };
        write_counters(&transaction, session, turn, &counters)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(record)
    }

    /// Count one correction occurrence. Returns how many corrections of that
    /// kind the open turn has had, this one included.
    pub fn count_correction(
        &self,
        session: &str,
        counter: ExhaustedCounter,
        occurrence: &str,
    ) -> Result<u32, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        let (turn, mut counters) = open_turn(&transaction, session)?.ok_or_else(|| {
            SessionWorkError::Invalid(format!("session {session} has no open turn"))
        })?;
        if !counters
            .corrections
            .iter()
            .any(|(kind, id)| *kind == counter && id == occurrence)
        {
            counters.corrections.push((counter, occurrence.to_string()));
            write_counters(&transaction, session, turn, &counters)?;
            transaction.commit().map_err(SessionWorkError::io)?;
        }
        Ok(counters.corrections(counter))
    }

    /// Items of a session with the requests that allocated them.
    pub fn items_with_requests(
        &self,
        session: &str,
    ) -> Result<Vec<(SessionItem, String)>, SessionWorkError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare("SELECT body, request FROM items WHERE session=?1 ORDER BY rowid")
            .map_err(SessionWorkError::corrupt)?;
        let rows = statement
            .query_map([session], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(SessionWorkError::corrupt)?;
        rows.map(|row| {
            let (body, request) = row.map_err(SessionWorkError::corrupt)?;
            Ok((decode(&body)?, request))
        })
        .collect()
    }

    /// The item a request allocated, allocating it on first sight. Unlike
    /// `allocate_item`, a later observation with a different label or policy
    /// returns the original: an item's policy is fixed when it is first seen.
    pub fn observe_item(
        &self,
        session: &str,
        request: &str,
        kind: ItemKind,
        label: &str,
        policy: FinishPolicy,
        now: DateTime<Utc>,
    ) -> Result<SessionItem, SessionWorkError> {
        {
            let connection = self.open()?;
            let existing: Option<String> = connection
                .query_row(
                    "SELECT body FROM items WHERE session=?1 AND request=?2",
                    params![session, request],
                    |row| row.get(0),
                )
                .optional()
                .map_err(SessionWorkError::corrupt)?;
            if let Some(existing) = existing {
                return decode(&existing);
            }
        }
        match self.allocate_item(session, request, kind, label, policy, now) {
            Err(SessionWorkError::Conflict(_)) => {
                let connection = self.open()?;
                let body: String = connection
                    .query_row(
                        "SELECT body FROM items WHERE session=?1 AND request=?2",
                        params![session, request],
                        |row| row.get(0),
                    )
                    .map_err(SessionWorkError::corrupt)?;
                decode(&body)
            }
            other => other,
        }
    }
}

fn open_turn(
    connection: &Connection,
    session: &str,
) -> Result<Option<(u32, TurnCounters)>, SessionWorkError> {
    let latest: Option<(u32, String)> = connection
        .query_row(
            "SELECT turn, body FROM turn_counters WHERE session=?1 ORDER BY turn DESC LIMIT 1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?;
    let Some((turn, body)) = latest else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare("SELECT body FROM turn_outcomes WHERE session=?1 AND turn=?2")
        .map_err(SessionWorkError::corrupt)?;
    let outcomes = statement
        .query_map(params![session, turn], |row| row.get::<_, String>(0))
        .map_err(SessionWorkError::corrupt)?;
    for outcome in outcomes {
        let outcome: TurnOutcome = decode(&outcome.map_err(SessionWorkError::corrupt)?)?;
        if outcome.settles_turn() {
            return Ok(None);
        }
    }
    Ok(Some((turn, decode(&body)?)))
}

fn write_counters(
    connection: &Connection,
    session: &str,
    turn: u32,
    counters: &TurnCounters,
) -> Result<(), SessionWorkError> {
    connection
        .execute(
            "UPDATE turn_counters SET body=?3 WHERE session=?1 AND turn=?2",
            params![session, turn, encode(counters)?],
        )
        .map_err(SessionWorkError::io)?;
    Ok(())
}

fn stops(connection: &Connection, session: &str) -> Result<Vec<StopRecord>, SessionWorkError> {
    let mut statement = connection
        .prepare("SELECT body FROM stops WHERE session=?1 ORDER BY number")
        .map_err(SessionWorkError::corrupt)?;
    let rows = statement
        .query_map([session], |row| row.get::<_, String>(0))
        .map_err(SessionWorkError::corrupt)?;
    rows.map(|row| decode(&row.map_err(SessionWorkError::corrupt)?))
        .collect()
}

fn current_stop(
    connection: &Connection,
    session: &str,
) -> Result<Option<StopRecord>, SessionWorkError> {
    let current: Vec<StopRecord> = stops(connection, session)?
        .into_iter()
        .filter(StopRecord::is_current)
        .collect();
    match current.len() {
        0 => Ok(None),
        1 => Ok(current.into_iter().next()),
        count => Err(SessionWorkError::Corrupt(format!(
            "session {session} has {count} current stop declarations"
        ))),
    }
}

fn read_stop(
    connection: &Connection,
    session: &str,
    number: u32,
) -> Result<Option<StopRecord>, SessionWorkError> {
    connection
        .query_row(
            "SELECT body FROM stops WHERE session=?1 AND number=?2",
            params![session, number],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?
        .map(|body| decode(&body))
        .transpose()
}

fn write_stop(
    connection: &Connection,
    session: &str,
    record: &StopRecord,
    settled_request: Option<&str>,
) -> Result<(), SessionWorkError> {
    let changed = connection
        .execute(
            "UPDATE stops SET body=?3, settled_request=coalesce(?4, settled_request)
             WHERE session=?1 AND number=?2",
            params![session, record.number, encode(record)?, settled_request],
        )
        .map_err(SessionWorkError::io)?;
    if changed != 1 {
        return Err(SessionWorkError::Corrupt(format!(
            "session {session} has no stop {} to update",
            record.number
        )));
    }
    Ok(())
}
