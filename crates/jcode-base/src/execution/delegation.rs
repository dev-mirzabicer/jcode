//! Atomic child admission and FIFO ordering over ordinary invocation records.
use super::ExecutionStore;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

/// Kernel-owned short transaction lease, shared by admission and human context
/// operations. It is never held while the human browses or the curator runs.
pub struct ChildControlLease {
    _file: std::fs::File,
}

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE runs_with_queue (
        id TEXT PRIMARY KEY,session_id TEXT NOT NULL,message_id TEXT NOT NULL,
        tool TEXT NOT NULL,input_digest TEXT NOT NULL,state TEXT NOT NULL,
        owner TEXT NOT NULL,input_path TEXT NOT NULL,result_path TEXT,output_path TEXT,
        output_bytes INTEGER NOT NULL DEFAULT 0,complete INTEGER NOT NULL DEFAULT 0,
        created INTEGER NOT NULL DEFAULT (unixepoch()),updated INTEGER NOT NULL DEFAULT (unixepoch()),
        background INTEGER NOT NULL DEFAULT 0,stop_cause TEXT,parent_id TEXT REFERENCES runs(id),
        progress TEXT,process_exit TEXT,superseded INTEGER NOT NULL DEFAULT 0,native_tracking INTEGER,
        CHECK(state IN ('prepared','queued','running','completed','failed','cancelled','interrupted'))
    );
    INSERT INTO runs_with_queue SELECT * FROM runs;
    DROP TABLE runs;
    ALTER TABLE runs_with_queue RENAME TO runs;
    CREATE INDEX runs_session_page ON runs(session_id,id);
    CREATE INDEX runs_state_page ON runs(state,created,id);
    CREATE TABLE child_turns (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT UNIQUE NOT NULL REFERENCES runs(id),
        child_id TEXT NOT NULL,
        cohort TEXT NOT NULL,
        cancelled_by TEXT REFERENCES runs(id),
        initial INTEGER NOT NULL CHECK(initial IN (0,1))
    );
    CREATE INDEX child_turns_fifo ON child_turns(child_id,sequence);
    CREATE UNIQUE INDEX child_creation_identity ON child_turns(child_id) WHERE initial=1;
    CREATE TRIGGER activity_run_created AFTER INSERT ON runs BEGIN
        INSERT INTO session_activity(session_id,last_active,generation) VALUES (NEW.session_id,NEW.created,1)
        ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1;
    END;
    CREATE TRIGGER activity_run_transition AFTER UPDATE OF state ON runs WHEN NEW.state<>OLD.state BEGIN
        INSERT INTO session_activity(session_id,last_active,generation) VALUES (NEW.session_id,NEW.updated,1)
        ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1;
        INSERT INTO session_activity(session_id,last_active,generation)
            SELECT child_id,NEW.updated,1 FROM child_turns WHERE run_id=NEW.id
        ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1;
        UPDATE child_turns SET cancelled_by=NEW.id
            WHERE cohort=(SELECT cohort FROM child_turns WHERE run_id=NEW.id)
              AND run_id<>NEW.id AND cancelled_by IS NULL
              AND (SELECT state FROM runs WHERE id=child_turns.run_id)='queued'
              AND OLD.state='running' AND NEW.state IN ('failed','cancelled','interrupted');
    END;
    PRAGMA user_version=20;")?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildAdmission {
    pub child_id: String,
    pub run_id: String,
    pub queued: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildTurnClaim {
    Waiting,
    Claimed,
    CancelledBy(String),
}

impl ExecutionStore {
    fn child_control_lease(&self, child: &str) -> Result<ChildControlLease> {
        ensure!(!child.is_empty(), "Child control requires an identity");
        let directory = self.root().join("child-controls");
        crate::storage::ensure_dir(&directory)?;
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            "Child control directory changed type"
        );
        let path = directory.join(format!("{:x}", Sha256::digest(child.as_bytes())));
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        file.try_lock()
            .context("Child control is busy; retry the requested action")?;
        Ok(ChildControlLease { _file: file })
    }

    /// Retain this guard through source capture or context persistence. A new
    /// turn cannot pass admission between the idle check and the transaction.
    pub fn idle_child_control(&self, child: &str) -> Result<ChildControlLease> {
        let lease = self.child_control_lease(child)?;
        ensure!(
            self.unfinished_child_turns(child)?.is_empty(),
            "Child is busy; context editing requires an idle child"
        );
        ensure!(
            self.child_foreground_work(child)?.is_empty(),
            "Child still owns foreground work"
        );
        Ok(lease)
    }

    /// Recover only proven-lost owners. Live owners require no per-turn body
    /// reads. Unproven work retains its reservation and produces diagnostics.
    pub async fn recover_abandoned_children(&self) -> Result<Vec<String>> {
        let owners = {
            let connection = self.connection()?;
            let mut query = connection.prepare("SELECT DISTINCT r.owner FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE r.state IN ('prepared','queued','running')")?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut issues = Vec::new();
        for owner in owners {
            let Some(runtime) = self.runtime_endpoint(&owner)? else {
                issues.push(format!("Child runtime {owner} has no verifiable owner"));
                continue;
            };
            if !runtime.image_is_gone(self)? {
                continue;
            }
            let ids = {
                let connection = self.connection()?;
                let mut query=connection.prepare("SELECT c.run_id FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE r.owner=?1 AND r.state IN ('prepared','queued','running') ORDER BY c.sequence")?;
                query
                    .query_map([&owner], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for id in ids {
                if let Err(error) = self.recover_lost_owner(&id).await {
                    issues.push(format!("{id}: {error:#}"));
                }
            }
        }
        Ok(issues)
    }

    pub(super) fn child_foreground_work(&self, child: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query=connection.prepare("SELECT id FROM runs WHERE session_id=?1 AND background=0 AND state IN ('prepared','queued','running') ORDER BY created,id")?;
        Ok(query
            .query_map([child], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }
    /// Capacity admission and child FIFO insertion share one write transaction.
    /// Inputs already belong to the invocation; queueing creates no second copy.
    pub fn admit_child_turn(
        &self,
        run_id: &str,
        owner: &str,
        child_id: &str,
        initial: bool,
        queue_if_busy: bool,
        limit: usize,
    ) -> Result<ChildAdmission> {
        ensure!(limit != 0, "Child capacity must be positive");
        let _control = self.child_control_lease(child_id)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (tool, actual_owner, state): (String, String, String) = tx.query_row(
            "SELECT tool,owner,state FROM runs WHERE id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ensure!(
            tool == "subagent"
                && actual_owner == owner
                && matches!(state.as_str(), "prepared" | "running" | "queued"),
            "Child admission lost its invocation ownership"
        );
        if let Some((existing, was_initial)) = tx
            .query_row(
                "SELECT child_id,initial FROM child_turns WHERE run_id=?1",
                [run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .optional()?
        {
            ensure!(
                existing == child_id && was_initial == initial,
                "Child invocation replay conflicts with original admission"
            );
            return Ok(ChildAdmission {
                child_id: existing,
                run_id: run_id.into(),
                queued: state == "queued",
            });
        }
        let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE c.child_id=?1 AND r.state IN ('prepared','queued','running'))", [child_id], |row| row.get(0))?;
        ensure!(
            !busy || (!initial && queue_if_busy),
            "Child is busy. Set queue_if_busy=true to append this follow-up to its FIFO."
        );
        if !busy {
            let active: i64 = tx.query_row("SELECT COUNT(DISTINCT c.child_id) FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE r.state IN ('prepared','queued','running')", [], |row| row.get(0))?;
            ensure!(
                u64::try_from(active)? < u64::try_from(limit)?,
                "Running-child capacity exceeded. Wait for an existing child, then retry explicitly. No capacity queue was created."
            );
        }
        let cohort: String = if busy {
            tx.query_row("SELECT c.cohort FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE c.child_id=?1 AND r.state IN ('prepared','queued','running') ORDER BY c.sequence LIMIT 1", [child_id], |row| row.get(0))?
        } else {
            run_id.into()
        };
        tx.execute(
            "INSERT INTO child_turns(run_id,child_id,initial,cohort) VALUES (?1,?2,?3,?4)",
            params![run_id, child_id, initial, cohort],
        )?;
        tx.execute(
            "UPDATE runs SET state=?2,updated=unixepoch() WHERE id=?1",
            params![run_id, if busy { "queued" } else { "running" }],
        )?;
        tx.execute("INSERT INTO session_activity(session_id,last_active,generation) VALUES (?1,unixepoch(),1) ON CONFLICT(session_id) DO UPDATE SET last_active=MAX(session_activity.last_active,excluded.last_active),generation=session_activity.generation+1", [child_id])?;
        tx.commit()?;
        Ok(ChildAdmission {
            child_id: child_id.into(),
            run_id: run_id.into(),
            queued: busy,
        })
    }

    /// A queued turn is claimed only when all earlier admitted work is terminal.
    pub fn claim_child_turn(&self, run_id: &str, owner: &str) -> Result<ChildTurnClaim> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (child, sequence, cancelled_by): (String, i64, Option<String>) = tx.query_row(
            "SELECT child_id,sequence,cancelled_by FROM child_turns WHERE run_id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if let Some(predecessor) = cancelled_by {
            return Ok(ChildTurnClaim::CancelledBy(predecessor));
        }
        let earlier: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE c.child_id=?1 AND c.sequence<?2 AND r.state IN ('prepared','queued','running'))", params![child,sequence], |row| row.get(0))?;
        if earlier {
            return Ok(ChildTurnClaim::Waiting);
        }
        let claimed = tx.execute("UPDATE runs SET state='running',updated=unixepoch() WHERE id=?1 AND owner=?2 AND state='queued' AND stop_cause IS NULL", params![run_id,owner])? == 1;
        tx.commit()?;
        ensure!(
            claimed,
            "Queued child turn lost ownership, was stopped or was already claimed"
        );
        Ok(ChildTurnClaim::Claimed)
    }

    pub fn child_for_run(&self, run_id: &str) -> Result<Option<String>> {
        self.connection()?
            .query_row(
                "SELECT child_id FROM child_turns WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()
            .context("Read child invocation relationship")
    }

    pub fn unfinished_child_turns(&self, child_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT c.run_id FROM child_turns c JOIN runs r ON r.id=c.run_id WHERE c.child_id=?1 AND r.state IN ('prepared','queued','running') ORDER BY c.sequence")?;
        Ok(statement
            .query_map([child_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Invocation, PreparedInvocation, RunRecord, RunState};
    use jcode_tool_types::ToolOutput;

    fn run(store: &ExecutionStore, name: &str) -> RunRecord {
        let invocation = Invocation {
            session_id: "parent".into(),
            message_id: name.into(),
            call_path: vec![name.into()],
            tool: "subagent".into(),
            input: serde_json::json!({"prompt":name}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&invocation, "owner").unwrap() else {
            panic!()
        };
        record
    }

    #[test]
    fn idle_context_lease_and_child_admission_are_mutually_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(temp.path()).unwrap();
        let first = run(&store, "first");
        let lease = store.idle_child_control("child").unwrap();
        assert!(
            store
                .admit_child_turn(&first.id, "owner", "child", true, false, 15)
                .is_err()
        );
        assert!(store.child_for_run(&first.id).unwrap().is_none());
        drop(lease);
        store
            .admit_child_turn(&first.id, "owner", "child", true, false, 15)
            .unwrap();
        assert!(store.idle_child_control("child").is_err());
        store
            .retain(first, ToolOutput::new("reply"), RunState::Completed)
            .unwrap();
        assert!(store.idle_child_control("child").is_ok());
    }

    #[test]
    fn concurrent_child_admission_cannot_exceed_fifteen_and_keeps_rejected_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(temp.path()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(20));
        let workers = (0..20)
            .map(|index| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let record = run(&store, &format!("call-{index}"));
                    barrier.wait();
                    let accepted = store
                        .admit_child_turn(
                            &record.id,
                            "owner",
                            &format!("child-{index}"),
                            true,
                            false,
                            15,
                        )
                        .is_ok();
                    assert!(store.invocation_input(&record.id).is_ok());
                    accepted
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>(),
            15
        );
        let count: i64 = store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM child_turns", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 15);
    }

    #[test]
    fn busy_opt_in_fifo_and_targeted_queue_cancellation_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(temp.path()).unwrap();
        let first = run(&store, "first");
        assert!(
            !store
                .admit_child_turn(&first.id, "owner", "child", true, false, 1)
                .unwrap()
                .queued
        );
        let second = run(&store, "second");
        assert!(
            store
                .admit_child_turn(&second.id, "owner", "child", false, false, 1)
                .is_err()
        );
        assert!(store.child_for_run(&second.id).unwrap().is_none());
        assert!(
            store
                .admit_child_turn(&second.id, "owner", "child", false, true, 1)
                .unwrap()
                .queued
        );
        let third = run(&store, "third");
        store
            .admit_child_turn(&third.id, "owner", "child", false, true, 1)
            .unwrap();
        assert_eq!(
            store.claim_child_turn(&third.id, "owner").unwrap(),
            ChildTurnClaim::Waiting
        );
        assert_eq!(
            store.inspect(&second.id).unwrap().unwrap().state,
            RunState::Queued
        );
        store
            .retain(
                second.clone(),
                ToolOutput::new("selected queue entry cancelled"),
                RunState::Cancelled,
            )
            .unwrap();
        assert_eq!(
            store.claim_child_turn(&third.id, "owner").unwrap(),
            ChildTurnClaim::Waiting
        );
        store
            .retain(first, ToolOutput::new("reply"), RunState::Completed)
            .unwrap();
        assert_eq!(
            store.claim_child_turn(&third.id, "owner").unwrap(),
            ChildTurnClaim::Claimed
        );
        store
            .retain(third, ToolOutput::new("reply"), RunState::Completed)
            .unwrap();
        let other = run(&store, "other");
        assert!(
            store
                .admit_child_turn(&other.id, "owner", "other-child", true, false, 1)
                .is_ok()
        );
        assert!(store.invocation_input(&second.id).unwrap().input["prompt"] == "second");
    }

    #[test]
    fn failed_running_turn_cancels_its_fifo_but_not_a_later_explicit_followup() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExecutionStore::open(temp.path()).unwrap();
        let first = run(&store, "first");
        store
            .admit_child_turn(&first.id, "owner", "child", true, false, 15)
            .unwrap();
        let second = run(&store, "second");
        store
            .admit_child_turn(&second.id, "owner", "child", false, true, 15)
            .unwrap();
        let first_id = first.id.clone();
        store
            .retain(first, ToolOutput::new("provider failed"), RunState::Failed)
            .unwrap();
        assert_eq!(
            store.claim_child_turn(&second.id, "owner").unwrap(),
            ChildTurnClaim::CancelledBy(first_id)
        );
        store
            .retain(
                second,
                ToolOutput::new("cancelled by predecessor"),
                RunState::Cancelled,
            )
            .unwrap();
        let next = run(&store, "explicit-next");
        assert!(
            !store
                .admit_child_turn(&next.id, "owner", "child", false, false, 15)
                .unwrap()
                .queued
        );
        assert!(
            !store
                .session_is_cold(
                    "child",
                    chrono::Utc::now().timestamp() + 2 * super::super::IDLE_SECONDS
                )
                .unwrap()
        );
    }
}
