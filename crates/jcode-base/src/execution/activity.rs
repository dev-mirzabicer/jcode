//! One durable operational clock. Metadata saves and polling never touch it.
use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::fs::{File, OpenOptions};

pub const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;

pub struct SessionActivityGuard {
    store: ExecutionStore,
    namespace: String,
    session: String,
    token: String,
    _lease: File,
}
impl Drop for SessionActivityGuard {
    fn drop(&mut self) {
        let result = (|| -> Result<()> {
            ensure!(
                self.store.provider_receipt_namespace()? == self.namespace,
                "Activity owner namespace changed; no completion was applied to the replacement store"
            );
            let mut connection = self.store.connection()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ExecutionStore::touch_activity_in(&tx, &self.session, chrono::Utc::now().timestamp())?;
            tx.execute(
                "DELETE FROM session_activity_leases WHERE token=?1 AND session_id=?2",
                params![self.token, self.session],
            )?;
            tx.commit()?;
            std::fs::remove_file(self.store.root().join("activity-leases").join(&self.token))?;
            Ok(())
        })();
        if let Err(error) = result {
            crate::logging::warn(&format!("Session activity finalization failed: {error}"));
        }
    }
}

impl ExecutionStore {
    pub(super) fn touch_activity_in(
        connection: &Connection,
        session: &str,
        now: i64,
    ) -> Result<()> {
        ensure!(
            !session.is_empty(),
            "Activity requires an exact session identity"
        );
        connection.execute("INSERT INTO session_activity(session_id,last_active,generation) VALUES (?1,?2,1) ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1", params![session, now])?;
        Ok(())
    }

    pub fn touch_activity(&self, session: &str, now: i64) -> Result<()> {
        Self::touch_activity_in(&self.connection()?, session, now)
    }

    pub fn last_activity(&self, session: &str) -> Result<Option<i64>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT last_active FROM session_activity WHERE session_id=?1",
                [session],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn begin_session_activity(&self, session: &str) -> Result<SessionActivityGuard> {
        let namespace = self.provider_receipt_namespace()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let directory = self.root().join("activity-leases");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Activity lease directory changed type"
        );
        jcode_core::fs::set_directory_permissions_owner_only(&directory)?;
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lease = options.open(directory.join(&token))?;
        lease.lock()?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::touch_activity_in(&tx, session, chrono::Utc::now().timestamp())?;
        tx.execute(
            "INSERT INTO session_activity_leases(token,session_id) VALUES (?1,?2)",
            params![token, session],
        )?;
        tx.commit()?;
        Ok(SessionActivityGuard {
            store: self.clone(),
            namespace,
            session: session.into(),
            token,
            _lease: lease,
        })
    }

    /// Removes only registered leases whose kernel owner is gone. Observation is
    /// not new activity, and unresolved execution runs continue to exclude idle.
    pub fn reconcile_activity_leases(&self, now: i64) -> Result<usize> {
        let connection = self.connection()?;
        let mut query =
            connection.prepare("SELECT token,stopped_observed_at FROM session_activity_leases")?;
        let tokens = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut removed = 0;
        for (token, stopped) in tokens {
            ensure!(
                token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid activity lease identity"
            );
            let path = self.root().join("activity-leases").join(&token);
            let mut options = OpenOptions::new();
            options.read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options.open(&path).context(
                "Activity ownership unavailable; session remains excluded from maintenance",
            )?;
            ensure!(file.metadata()?.is_file(), "Activity lease changed type");
            match file.try_lock() {
                Ok(()) => {
                    // A dead owner's exact finish time is unavailable. Remember
                    // the first proof it stopped, without changing last_active.
                    // Repeated scans never refresh this bound or postpone it.
                    let Some(stopped) = stopped else {
                        connection.execute("UPDATE session_activity_leases SET stopped_observed_at=?2 WHERE token=?1 AND stopped_observed_at IS NULL", params![token,now])?;
                        continue;
                    };
                    if stopped > now.saturating_sub(IDLE_SECONDS) {
                        continue;
                    }
                    removed += connection.execute(
                        "DELETE FROM session_activity_leases WHERE token=?1",
                        [&token],
                    )?;
                    std::fs::remove_file(&path)?;
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Ok(removed)
    }

    pub(super) fn session_is_cold(&self, session: &str, now: i64) -> Result<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM session_activity a WHERE a.session_id=?1 AND a.last_active<=?2 AND a.retention_not_before<=?3 AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.session_id=a.session_id AND r.state IN ('prepared','queued','running')) AND NOT EXISTS(SELECT 1 FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE c.child_id=a.session_id AND r.state IN ('prepared','queued','running')) AND NOT EXISTS(SELECT 1 FROM session_activity_leases l WHERE l.session_id=a.session_id))", params![session, now.saturating_sub(IDLE_SECONDS), now], |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_observes_a_full_idle_week_without_fabricating_legacy_activity() -> Result<()> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("execution");
        std::fs::create_dir(&directory)?;
        let historical = rusqlite::Connection::open(directory.join("index.sqlite"))?;
        historical.execute_batch(include_str!("fixtures/schema-18.sql"))?;
        historical.execute("INSERT INTO session_activity(session_id,last_active,generation) VALUES('legacy',100,1)", [])?;
        drop(historical);
        let now = chrono::Utc::now().timestamp();
        let migrated = ExecutionStore::open(root.path())?;
        assert_eq!(migrated.last_activity("legacy")?, Some(100));
        assert!(!migrated.session_is_cold("legacy", now)?);
        let floor: i64 = migrated.connection()?.query_row(
            "SELECT retention_not_before FROM session_activity WHERE session_id='legacy'",
            [],
            |row| row.get(0),
        )?;
        assert!(floor >= now + IDLE_SECONDS);
        assert!(migrated.session_is_cold("legacy", floor)?);
        assert_eq!(
            ExecutionStore::open(root.path())?.connection()?.query_row(
                "SELECT retention_not_before FROM session_activity WHERE session_id='legacy'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            floor
        );
        Ok(())
    }

    #[test]
    fn maintenance_and_lost_lease_observation_do_not_refresh_actual_activity() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        store.touch_activity("reader", 100)?;
        let now = 100 + IDLE_SECONDS;
        assert!(store.session_is_cold("reader", now)?);
        store.maintain_retention(&Default::default(), now)?;
        store.retention_status()?;
        store.list("reader", None, 50)?;
        assert_eq!(store.last_activity("reader")?, Some(100));
        let token = "11111111111111111111111111111111";
        let directory = store.root().join("activity-leases");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join(token), b"")?;
        store.connection()?.execute(
            "INSERT INTO session_activity_leases(token,session_id) VALUES (?1,'reader')",
            [token],
        )?;
        assert_eq!(store.reconcile_activity_leases(now)?, 0);
        assert!(!store.session_is_cold("reader", now)?);
        assert_eq!(store.reconcile_activity_leases(now + IDLE_SECONDS - 1)?, 0);
        assert_eq!(store.last_activity("reader")?, Some(100));
        assert_eq!(store.reconcile_activity_leases(now + IDLE_SECONDS)?, 1);
        assert!(store.session_is_cold("reader", now + IDLE_SECONDS)?);
        Ok(())
    }

    #[test]
    fn live_activity_lease_excludes_quiet_model_work_until_actual_completion() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let guard = store.begin_session_activity("active")?;
        let started = store.last_activity("active")?.unwrap();
        assert!(!store.session_is_cold("active", started + IDLE_SECONDS + 1)?);
        assert_eq!(
            store.reconcile_activity_leases(started + IDLE_SECONDS + 1)?,
            0
        );
        drop(guard);
        assert!(store.last_activity("active")?.unwrap() >= started);
        let count: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM session_activity_leases",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn old_activity_completion_cannot_write_into_a_replacement_namespace() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = ExecutionStore::open(root.path())?;
        let guard = store.begin_session_activity("old")?;
        std::fs::rename(store.root(), root.path().join("old-execution"))?;
        let replacement = ExecutionStore::open(root.path())?;
        drop(guard);
        assert!(replacement.last_activity("old")?.is_none());
        Ok(())
    }
}
