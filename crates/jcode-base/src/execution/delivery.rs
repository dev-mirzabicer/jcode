use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    InFlight,
    Delivered,
    Uncertain,
    /// Terminal: the recipient Session does not exist.
    Undeliverable,
}
impl DeliveryState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "delivered" => Ok(Self::Delivered),
            "uncertain" => Ok(Self::Uncertain),
            "undeliverable" => Ok(Self::Undeliverable),
            _ => anyhow::bail!("Invalid delivery state"),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundDelivery {
    pub run_id: String,
    pub notify: bool,
    pub wake: bool,
    pub started_at: String,
    pub notify_state: DeliveryState,
    pub wake_state: DeliveryState,
    /// The result always goes to the agent, whatever `wake` says: its
    /// session has session work, where `wake` is the item's wake or hold
    /// policy and the arrival rule decides.
    #[serde(default)]
    pub agent: bool,
    /// Why the delivery is undeliverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}
/// The notify channel is enabled.
const NOTIFY_ENABLED: &str = "notify=1";
/// The wake channel is enabled: the result wakes its session, or it always
/// goes to the agent (a session with session work) and the session-work
/// arrival rule decides whether it wakes.
const WAKE_ENABLED: &str = "(wake=1 OR agent=1)";

#[derive(Clone, Copy)]
pub enum DeliveryChannel {
    Notify,
    Wake,
}

pub struct DeliveryAttempt {
    store: ExecutionStore,
    id: String,
    channel: DeliveryChannel,
    token: String,
    lease: Option<std::fs::File>,
    finished: bool,
}
impl DeliveryAttempt {
    pub fn finish(mut self, delivered: bool) -> Result<()> {
        self.store
            .finish_delivery(&self.id, self.channel, &self.token, delivered)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for DeliveryAttempt {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let store = self.store.clone();
        let id = self.id.clone();
        let channel = self.channel;
        let token = self.token.clone();
        let lease = self.lease.take();
        let finish = move || {
            let _ = store.mark_delivery_uncertain(&id, channel, &token);
            drop(lease);
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(finish);
        } else {
            finish();
        }
    }
}
impl ExecutionStore {
    pub fn unfinished_background_deliveries(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query=connection.prepare("SELECT d.run_id FROM background_deliveries d JOIN runs r ON r.id=d.run_id WHERE r.state IN ('prepared','queued','running') ORDER BY d.run_id")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn execution_times(&self, id: &str) -> Result<(String, Option<String>, Option<f64>)> {
        let (created, updated, state): (i64, i64, String) = self.connection()?.query_row(
            "SELECT created,updated,state FROM runs WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let started = chrono::DateTime::from_timestamp(created, 0)
            .context("Invalid execution timestamp")?
            .to_rfc3339();
        let ended = !matches!(state.as_str(), "prepared" | "running");
        Ok((
            started,
            if ended {
                Some(
                    chrono::DateTime::from_timestamp(updated, 0)
                        .context("Invalid completion timestamp")?
                        .to_rfc3339(),
                )
            } else {
                None
            },
            ended.then_some((updated - created).max(0) as f64),
        ))
    }
    pub fn pending_background_deliveries(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query=connection.prepare("SELECT d.run_id FROM background_deliveries d JOIN runs r ON r.id=d.run_id WHERE r.state IN ('completed','failed','cancelled','interrupted') AND ((d.notify=1 AND d.notify_state='pending') OR ((d.wake=1 OR d.agent=1) AND d.wake_state='pending'))")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }
    pub fn register_background_delivery(
        &self,
        id: &str,
        notify: bool,
        wake: bool,
    ) -> Result<(BackgroundDelivery, bool)> {
        self.register_delivery(id, notify, wake, false)
    }
    /// Register a delivery whose result always goes to the agent: `wake` is
    /// the item's policy, which the session-work arrival rule applies.
    pub fn register_agent_delivery(
        &self,
        id: &str,
        notify: bool,
        wake: bool,
    ) -> Result<(BackgroundDelivery, bool)> {
        self.register_delivery(id, notify, wake, true)
    }
    fn register_delivery(
        &self,
        id: &str,
        notify: bool,
        wake: bool,
        agent: bool,
    ) -> Result<(BackgroundDelivery, bool)> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        ensure!(exists, "Background delivery requires a durable invocation");
        let created=transaction.execute("INSERT OR IGNORE INTO background_deliveries (run_id,notify,wake,started_at,agent) VALUES (?1,?2,?3,?4,?5)",params![id,notify||wake,wake,chrono::Utc::now().to_rfc3339(),agent])?==1;
        transaction.commit()?;
        Ok((
            self.background_delivery(id)?
                .context("Delivery registration disappeared")?,
            created,
        ))
    }
    pub fn background_delivery(&self, id: &str) -> Result<Option<BackgroundDelivery>> {
        type Row = (bool, bool, String, String, String, Option<String>, bool);
        let row: Option<Row> = self
            .connection()?
            .query_row(
                "SELECT notify,wake,started_at,notify_state,wake_state,issue,agent FROM background_deliveries WHERE run_id=?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(notify, wake, started_at, n, w, issue, agent)| {
            Ok(BackgroundDelivery {
                run_id: id.into(),
                notify,
                wake,
                started_at,
                notify_state: DeliveryState::parse(&n)?,
                wake_state: DeliveryState::parse(&w)?,
                agent,
                issue,
            })
        })
        .transpose()
    }
    /// Settle every enabled channel still pending or uncertain as terminal
    /// and undeliverable, with the reason. Channels already delivered or in
    /// flight keep their state. Returns whether anything changed.
    pub fn settle_undeliverable(&self, id: &str, issue: &str) -> Result<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for (enabled, state) in [
            (NOTIFY_ENABLED, "notify_state"),
            (WAKE_ENABLED, "wake_state"),
        ] {
            changed += transaction.execute(
                &format!(
                    "UPDATE background_deliveries SET {state}='undeliverable',issue=?2 WHERE run_id=?1 AND {enabled} AND {state} IN ('pending','uncertain')"
                ),
                params![id, issue],
            )?;
        }
        transaction.commit()?;
        Ok(changed > 0)
    }
    pub fn background_delivery_ids(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_id FROM background_deliveries ORDER BY started_at DESC,run_id")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }
    /// Change a delivery's flags. An agent delivery's `wake` is its item's
    /// policy, fixed when the work started, so only its notify flag changes.
    pub fn update_background_delivery(&self, id: &str, notify: bool, wake: bool) -> Result<()> {
        ensure!(
            self.connection()?.execute(
                "UPDATE background_deliveries SET notify=?2,wake=CASE WHEN agent=1 THEN wake ELSE ?3 END WHERE run_id=?1",
                params![id, notify || wake, wake]
            )? == 1,
            "Unknown background delivery"
        );
        Ok(())
    }
    pub fn begin_delivery(
        &self,
        id: &str,
        channel: DeliveryChannel,
    ) -> Result<Option<DeliveryAttempt>> {
        let (enabled, state, attempt) = match channel {
            DeliveryChannel::Notify => (NOTIFY_ENABLED, "notify_state", "notify_attempt"),
            DeliveryChannel::Wake => (WAKE_ENABLED, "wake_state", "wake_attempt"),
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let directory = self.root().join("delivery-attempts");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Delivery attempt directory changed type"
        );
        let path = directory.join(format!("{token}.lease"));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lease = options.open(&path)?;
        lease.try_lock()?;
        lease.sync_all()?;
        #[cfg(unix)]
        std::fs::File::open(&directory)?.sync_all()?;
        let changed=self.connection()?.execute(&format!("UPDATE background_deliveries SET {state}='in_flight',{attempt}=?2 WHERE run_id=?1 AND {enabled} AND {state}='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND state IN ('completed','failed','cancelled','interrupted'))"),params![id,token])?;
        if changed == 1 {
            Ok(Some(DeliveryAttempt {
                store: self.clone(),
                id: id.into(),
                channel,
                token,
                lease: Some(lease),
                finished: false,
            }))
        } else {
            drop(lease);
            std::fs::remove_file(path)?;
            Ok(None)
        }
    }
    pub fn finish_delivery(
        &self,
        id: &str,
        channel: DeliveryChannel,
        token: &str,
        delivered: bool,
    ) -> Result<()> {
        let (state, attempt) = match channel {
            DeliveryChannel::Notify => ("notify_state", "notify_attempt"),
            DeliveryChannel::Wake => ("wake_state", "wake_attempt"),
        };
        ensure!(self.connection()?.execute(&format!("UPDATE background_deliveries SET {state}=?3,{attempt}=NULL WHERE run_id=?1 AND {attempt}=?2 AND {state}='in_flight'"),params![id,token,if delivered{"delivered"}else{"pending"}])?==1,"Delivery acknowledgement no longer matches the active attempt");
        Ok(())
    }
    /// An interrupted delivery may already have reached its recipient. Do not
    /// automatically repeat model wakeups when their acknowledgement is lost.
    pub fn mark_delivery_uncertain(
        &self,
        id: &str,
        channel: DeliveryChannel,
        token: &str,
    ) -> Result<()> {
        let (state, attempt) = match channel {
            DeliveryChannel::Notify => ("notify_state", "notify_attempt"),
            DeliveryChannel::Wake => ("wake_state", "wake_attempt"),
        };
        self.connection()?.execute(&format!("UPDATE background_deliveries SET {state}='uncertain' WHERE run_id=?1 AND {attempt}=?2 AND {state}='in_flight'"),params![id,token])?;
        Ok(())
    }

    pub fn reconcile_delivery_attempts(&self) -> Result<usize> {
        let connection = self.connection()?;
        let mut query=connection.prepare("SELECT run_id,notify_attempt,wake_attempt,notify_state,wake_state FROM background_deliveries WHERE notify_state='in_flight' OR wake_state='in_flight'")?;
        let attempts = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut changed = 0;
        for (id, notify, wake, notify_state, wake_state) in attempts {
            for (channel, token, state) in [
                (DeliveryChannel::Notify, notify, notify_state),
                (DeliveryChannel::Wake, wake, wake_state),
            ] {
                if state != "in_flight" {
                    continue;
                }
                let token = token.context("In-flight delivery has no attempt identity")?;
                ensure!(
                    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "Invalid delivery attempt identity"
                );
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let file = options.open(
                    self.root()
                        .join("delivery-attempts")
                        .join(format!("{token}.lease")),
                )?;
                match file.try_lock() {
                    Ok(()) => {
                        self.mark_delivery_uncertain(&id, channel, &token)?;
                        changed += 1;
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation, RunState};
    fn completed(store: &ExecutionStore) -> Result<String> {
        let input = Invocation {
            session_id: "delivery".into(),
            message_id: "message".into(),
            call_path: vec![uuid::Uuid::new_v4().to_string()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(mut record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        store.promote(&record.id, "owner")?;
        store.register_background_delivery(&record.id, true, true)?;
        record.state = RunState::Completed;
        store.finish(&record)?;
        Ok(record.id)
    }
    #[test]
    fn failed_delivery_stays_pending_and_success_is_not_repeated() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        let first = store.begin_delivery(&id, DeliveryChannel::Notify)?.unwrap();
        assert!(
            store
                .begin_delivery(&id, DeliveryChannel::Notify)?
                .is_none()
        );
        assert_eq!(store.reconcile_delivery_attempts()?, 0);
        first.finish(false)?;
        let second = store.begin_delivery(&id, DeliveryChannel::Notify)?.unwrap();
        second.finish(true)?;
        assert!(
            store
                .begin_delivery(&id, DeliveryChannel::Notify)?
                .is_none()
        );
        assert_eq!(
            store.background_delivery(&id)?.unwrap().notify_state,
            DeliveryState::Delivered
        );
        assert_eq!(
            store.background_delivery(&id)?.unwrap().wake_state,
            DeliveryState::Pending
        );
        Ok(())
    }
    #[test]
    fn abandoned_dispatch_is_uncertain_and_never_automatically_wakes_again() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        let mut attempt = store.begin_delivery(&id, DeliveryChannel::Wake)?.unwrap();
        // Simulate a process image disappearing without running Drop.
        attempt.finished = true;
        drop(attempt);
        assert_eq!(store.reconcile_delivery_attempts()?, 1);
        assert_eq!(
            store.background_delivery(&id)?.unwrap().wake_state,
            DeliveryState::Uncertain
        );
        assert!(store.begin_delivery(&id, DeliveryChannel::Wake)?.is_none());
        Ok(())
    }
    #[test]
    fn registration_retry_preserves_later_delivery_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        store.update_background_delivery(&id, false, false)?;
        let (record, created) = store.register_background_delivery(&id, true, true)?;
        assert!(!created);
        assert!(!record.notify && !record.wake);
        assert!(store.pending_background_deliveries()?.is_empty());
        Ok(())
    }
    #[test]
    fn a_missing_recipient_settles_terminal_and_visible_without_touching_delivered_channels()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        // The notify channel was already delivered; only the pending one
        // settles.
        store
            .begin_delivery(&id, DeliveryChannel::Notify)?
            .unwrap()
            .finish(true)?;
        assert!(store.settle_undeliverable(&id, "recipient session gone does not exist")?);
        let record = store.background_delivery(&id)?.unwrap();
        assert_eq!(record.notify_state, DeliveryState::Delivered);
        assert_eq!(record.wake_state, DeliveryState::Undeliverable);
        assert_eq!(
            record.issue.as_deref(),
            Some("recipient session gone does not exist")
        );
        // Terminal: nothing is pending, nothing can be claimed, and settling
        // again changes nothing.
        assert!(store.pending_background_deliveries()?.is_empty());
        assert!(store.begin_delivery(&id, DeliveryChannel::Wake)?.is_none());
        assert!(!store.settle_undeliverable(&id, "again")?);
        // The task shows it.
        let status = crate::background::managed_projection(&store, &id)?.unwrap();
        assert!(status.event_history.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("Undeliverable: recipient session gone"))
        }));
        Ok(())
    }
    #[test]
    fn an_uncertain_channel_also_settles_when_its_recipient_is_gone() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        let mut attempt = store.begin_delivery(&id, DeliveryChannel::Wake)?.unwrap();
        attempt.finished = true;
        drop(attempt);
        store.reconcile_delivery_attempts()?;
        assert!(store.settle_undeliverable(&id, "gone")?);
        let record = store.background_delivery(&id)?.unwrap();
        assert_eq!(record.wake_state, DeliveryState::Undeliverable);
        assert_eq!(record.notify_state, DeliveryState::Undeliverable);
        Ok(())
    }
    #[test]
    fn version_21_deliveries_survive_the_rebuild_that_adds_undeliverable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let id = completed(&store)?;
        store
            .begin_delivery(&id, DeliveryChannel::Notify)?
            .unwrap()
            .finish(true)?;
        // Put the table back in its exact version-8 shape, as a version-21
        // store holds it.
        let connection =
            rusqlite::Connection::open(directory.path().join("execution/index.sqlite"))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=OFF;
            CREATE TABLE old_deliveries (
                run_id TEXT PRIMARY KEY REFERENCES runs(id), notify INTEGER NOT NULL,
                wake INTEGER NOT NULL, started_at TEXT NOT NULL,
                notify_state TEXT NOT NULL DEFAULT 'pending', wake_state TEXT NOT NULL DEFAULT 'pending',
                notify_attempt TEXT, wake_attempt TEXT,
                CHECK(notify_state IN ('pending','in_flight','delivered','uncertain')),
                CHECK(wake_state IN ('pending','in_flight','delivered','uncertain'))
            );
            INSERT INTO old_deliveries SELECT run_id,notify,wake,started_at,notify_state,wake_state,notify_attempt,wake_attempt FROM background_deliveries;
            DROP TABLE background_deliveries;
            ALTER TABLE old_deliveries RENAME TO background_deliveries;
            PRAGMA user_version=21;",
        )?;
        drop(connection);
        let store = ExecutionStore::open(directory.path())?;
        let record = store.background_delivery(&id)?.unwrap();
        assert_eq!(record.notify_state, DeliveryState::Delivered);
        assert_eq!(record.wake_state, DeliveryState::Pending);
        assert_eq!(record.issue, None);
        assert_eq!(store.pending_background_deliveries()?, vec![id.clone()]);
        assert!(store.settle_undeliverable(&id, "gone")?);
        let version: i64 =
            rusqlite::Connection::open(directory.path().join("execution/index.sqlite"))?
                .pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 22);
        Ok(())
    }
    #[test]
    fn an_agent_delivery_goes_to_the_agent_whatever_its_wake_policy() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = ExecutionStore::open(directory.path())?;
        let input = Invocation {
            session_id: "agent".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(mut record) = store.prepare(&input, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        store.promote(&record.id, "owner")?;
        // A hold item that does not notify still reaches the agent.
        let (delivery, created) = store.register_agent_delivery(&record.id, false, false)?;
        assert!(created && delivery.agent && !delivery.wake && !delivery.notify);
        // Its policy is fixed: a later update changes only notify.
        store.update_background_delivery(&record.id, true, true)?;
        let delivery = store.background_delivery(&record.id)?.unwrap();
        assert!(delivery.notify && !delivery.wake && delivery.agent);
        record.state = RunState::Completed;
        store.finish(&record)?;
        assert_eq!(
            store.pending_background_deliveries()?,
            vec![record.id.clone()]
        );
        let attempt = store
            .begin_delivery(&record.id, DeliveryChannel::Wake)?
            .expect("the agent channel is enabled");
        attempt.finish(true)?;
        assert_eq!(
            store.background_delivery(&record.id)?.unwrap().wake_state,
            DeliveryState::Delivered
        );
        Ok(())
    }
}
