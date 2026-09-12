//! Native helper provenance, not a second process supervisor. The command owner
//! proves spawn failure/quiescence; recovery only inspects and never guesses a PID.
use super::ExecutionStore;
#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, ensure};
use rusqlite::{TransactionBehavior, params};

impl ExecutionStore {
    pub(super) fn begin_native_process(&self, run: &str, owner: &str) -> Result<String> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner=?2 AND state='running')",
            params![run, owner],
            |row| row.get(0),
        )?;
        ensure!(owned, "Native process launch lost execution ownership");
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        transaction.execute(
            "INSERT INTO native_processes(ticket,run_id,owner) VALUES(?1,?2,?3)",
            params![ticket, run, owner],
        )?;
        transaction.commit()?;
        Ok(ticket)
    }

    pub(super) fn register_native_process(
        &self,
        run: &str,
        owner: &str,
        ticket: &str,
        pid: u32,
    ) -> Result<()> {
        ensure!(
            pid > 1 && pid != std::process::id(),
            "Refusing to register the runtime as its own native helper"
        );
        #[cfg(unix)]
        let (identity, identity_error) = match super::process::ProcessIdentity::capture(pid) {
            Ok(identity) => {
                ensure!(
                    identity.group == pid,
                    "Native helper is not a private process-group leader"
                );
                (Some(serde_json::to_string(&identity)?), None)
            }
            // Very short-lived children can already be zombies. Retain the
            // failed identity observation, never fabricate a birth identity or
            // authorize signalling from this numeric PID alone.
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        #[cfg(not(unix))]
        let (identity, identity_error): (Option<String>, Option<String>) =
            (None, Some("Native process identity is unsupported".into()));
        let changed=self.connection()?.execute("UPDATE native_processes SET pid=?4,identity=?5,identity_error=?6 WHERE ticket=?1 AND run_id=?2 AND owner=?3 AND pid IS NULL AND finished=0 AND EXISTS(SELECT 1 FROM runs WHERE id=?2 AND owner=?3 AND state='running')",params![ticket,run,owner,pid,identity,identity_error])?;
        ensure!(
            changed == 1,
            "Native process registration conflicts with its launch receipt"
        );
        Ok(())
    }

    pub(super) fn finish_native_process(&self, run: &str, owner: &str, ticket: &str) -> Result<()> {
        let changed=self.connection()?.execute("UPDATE native_processes SET finished=1 WHERE ticket=?1 AND run_id=?2 AND owner=?3 AND EXISTS(SELECT 1 FROM runs WHERE id=?2 AND owner=?3 AND state='running')",params![ticket,run,owner])?;
        ensure!(
            changed == 1,
            "Native process completion lost execution ownership"
        );
        Ok(())
    }

    pub(super) fn ensure_native_processes_finished(&self, run: &str) -> Result<()> {
        let pending: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_processes WHERE run_id=?1 AND finished=0)",
            [run],
            |row| row.get(0),
        )?;
        ensure!(
            !pending,
            "Native helper work has not been proven quiescent; terminal capture was not published"
        );
        Ok(())
    }

    #[cfg(unix)]
    pub(super) async fn verify_lost_native_processes(&self, run: &str) -> Result<()> {
        let store = self.clone();
        let id = run.to_string();
        let pending=tokio::task::spawn_blocking(move||->Result<Vec<(String,Option<u32>)>>{
            let connection=store.connection()?;
            let tracked:Option<i64>=connection.query_row("SELECT native_tracking FROM runs WHERE id=?1",[&id],|row|row.get(0))?;
            ensure!(tracked==Some(1),"Legacy execution has no complete native process provenance; no helper quiescence was inferred");
            let mut statement=connection.prepare("SELECT ticket,pid FROM native_processes WHERE run_id=?1 AND finished=0 ORDER BY ticket")?;
            Ok(statement.query_map([id],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?)
        }).await??;
        for (ticket, pid) in pending {
            let pid=pid.with_context(||format!("Execution {run} has an unproven native launch {ticket}; no process identity or completion was invented"))?;
            ensure!(
                !crate::platform::process_group_has_live_members(pid).await?,
                "Execution {run} lost its owner but a recorded native helper group still has live members. No completion was claimed and no PID was signalled."
            );
        }
        Ok(())
    }
}
