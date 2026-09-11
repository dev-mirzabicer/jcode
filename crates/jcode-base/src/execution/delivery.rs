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
}
impl DeliveryState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "delivered" => Ok(Self::Delivered),
            "uncertain" => Ok(Self::Uncertain),
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
}
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
        let mut query=connection.prepare("SELECT d.run_id FROM background_deliveries d JOIN runs r ON r.id=d.run_id WHERE r.state IN ('completed','failed','cancelled','interrupted') AND ((d.notify=1 AND d.notify_state='pending') OR (d.wake=1 AND d.wake_state='pending'))")?;
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
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        ensure!(exists, "Background delivery requires a durable invocation");
        let created=transaction.execute("INSERT OR IGNORE INTO background_deliveries (run_id,notify,wake,started_at) VALUES (?1,?2,?3,?4)",params![id,notify||wake,wake,chrono::Utc::now().to_rfc3339()])?==1;
        transaction.commit()?;
        Ok((
            self.background_delivery(id)?
                .context("Delivery registration disappeared")?,
            created,
        ))
    }
    pub fn background_delivery(&self, id: &str) -> Result<Option<BackgroundDelivery>> {
        let row:Option<(bool,bool,String,String,String)>=self.connection()?.query_row("SELECT notify,wake,started_at,notify_state,wake_state FROM background_deliveries WHERE run_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        row.map(|(notify, wake, started_at, n, w)| {
            Ok(BackgroundDelivery {
                run_id: id.into(),
                notify,
                wake,
                started_at,
                notify_state: DeliveryState::parse(&n)?,
                wake_state: DeliveryState::parse(&w)?,
            })
        })
        .transpose()
    }
    pub fn background_delivery_ids(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_id FROM background_deliveries ORDER BY started_at DESC,run_id")?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }
    pub fn update_background_delivery(&self, id: &str, notify: bool, wake: bool) -> Result<()> {
        ensure!(
            self.connection()?.execute(
                "UPDATE background_deliveries SET notify=?2,wake=?3 WHERE run_id=?1",
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
        let (flag, state, attempt) = match channel {
            DeliveryChannel::Notify => ("notify", "notify_state", "notify_attempt"),
            DeliveryChannel::Wake => ("wake", "wake_state", "wake_attempt"),
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
        let changed=self.connection()?.execute(&format!("UPDATE background_deliveries SET {state}='in_flight',{attempt}=?2 WHERE run_id=?1 AND {flag}=1 AND {state}='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND state IN ('completed','failed','cancelled','interrupted'))"),params![id,token])?;
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
}
