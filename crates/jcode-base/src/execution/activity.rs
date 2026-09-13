//! One durable operational clock. Metadata saves and polling never touch it.
use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::fs::{File, OpenOptions};

pub const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;

pub struct SessionActivityGuard {
    store: ExecutionStore,
    session: String,
    token: String,
    _lease: File,
}
impl Drop for SessionActivityGuard {
    fn drop(&mut self) {
        let result = (|| -> Result<()> {
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
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM session_activity a WHERE a.session_id=?1 AND a.last_active<=?2 AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.session_id=a.session_id AND r.state IN ('prepared','running')) AND NOT EXISTS(SELECT 1 FROM session_activity_leases l WHERE l.session_id=a.session_id))", params![session, now.saturating_sub(IDLE_SECONDS)], |row| row.get(0))?)
    }
}
