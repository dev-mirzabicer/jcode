//! Immutable provider-result acquisition receipts, distinct from tool execution.
//! These retain already-received effects/results before transcript correlation.
use super::{ExecutionStore, RunRecord};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderReceipt {
    pub sequence: i64,
    pub run_id: String,
    pub session_id: String,
    pub request_id: String,
    pub request_lease: PathBuf,
    pub tool_use_id: String,
    pub message_id: Option<String>,
}
impl ProviderReceipt {
    pub fn request_is_live(&self, store: &ExecutionStore) -> Result<bool> {
        ensure!(
            self.request_id.len() == 32
                && self.request_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid provider request identity"
        );
        ensure!(
            self.request_lease
                == store
                    .root()
                    .join("provider-requests")
                    .join(format!("{}.lease", self.request_id)),
            "Provider request lease differs from its metadata namespace"
        );
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&self.request_lease)
            .context("Provider request lease is unavailable; no abandoned request was inferred")?;
        ensure!(
            file.metadata()?.is_file(),
            "Provider request lease changed type"
        );
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}
impl ExecutionStore {
    pub fn validate_provider_receipt_watermark(&self, session: &str, sequence: i64) -> Result<()> {
        ensure!(sequence >= 0, "Invalid provider receipt watermark");
        if sequence > 0 {
            let present:bool=self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM provider_result_receipts WHERE session_id=?1 AND sequence=?2)",params![session,sequence],|row|row.get(0))?;
            ensure!(
                present,
                "Provider receipt metadata predates the acknowledged Session; no receipt was silently skipped"
            );
        }
        Ok(())
    }
    pub fn provider_receipt_namespace(&self) -> Result<String> {
        Ok(self.connection()?.query_row(
            "SELECT namespace FROM provider_receipt_namespace WHERE id=1",
            [],
            |row| row.get(0),
        )?)
    }
    pub fn register_provider_receipt(
        &self,
        record: &RunRecord,
        request_id: &str,
        lease: &std::path::Path,
        tool_use_id: &str,
    ) -> Result<ProviderReceipt> {
        ensure!(
            record.tool == "provider_result_receipt",
            "Provider acquisition receipt is not a tool execution"
        );
        let current = self
            .inspect(&record.id)?
            .context("Provider acquisition has no prepared record")?;
        ensure!(
            current.owner == record.owner
                && current.session_id == record.session_id
                && current.tool == record.tool
                && !current.state.terminal(),
            "Provider acquisition differs from its authoritative owner or recipient"
        );
        let connection = self.connection()?;
        connection.execute("INSERT INTO provider_result_receipts(run_id,session_id,request_id,request_lease,tool_use_id) VALUES (?1,?2,?3,?4,?5)",params![record.id,record.session_id,request_id,lease.to_str().context("Non-UTF-8 provider lease")?,tool_use_id])?;
        self.provider_receipts_after(&record.session_id, connection.last_insert_rowid() - 1)?
            .into_iter()
            .find(|receipt| receipt.run_id == record.id)
            .context("Provider receipt was not published")
    }
    pub fn provider_receipts_after(
        &self,
        session: &str,
        sequence: i64,
    ) -> Result<Vec<ProviderReceipt>> {
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT sequence,run_id,session_id,request_id,request_lease,tool_use_id,message_id FROM provider_result_receipts WHERE session_id=?1 AND sequence>?2 ORDER BY sequence")?;
        Ok(statement
            .query_map(params![session, sequence], |row| {
                Ok(ProviderReceipt {
                    sequence: row.get(0)?,
                    run_id: row.get(1)?,
                    session_id: row.get(2)?,
                    request_id: row.get(3)?,
                    request_lease: PathBuf::from(row.get::<_, String>(4)?),
                    tool_use_id: row.get(5)?,
                    message_id: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn correlate_provider_receipt(
        &self,
        reference: &jcode_tool_types::ProviderReceiptReference,
        session: &str,
        tool_use_id: &str,
        message: &str,
    ) -> Result<()> {
        ensure!(
            reference.namespace == self.provider_receipt_namespace()?,
            "Provider receipt belongs to an unknown or different metadata namespace"
        );
        let changed=self.connection()?.execute("UPDATE provider_result_receipts SET message_id=?5 WHERE run_id=?1 AND sequence=?2 AND session_id=?3 AND tool_use_id=?4 AND (message_id IS NULL OR message_id=?5)",params![reference.run_id,reference.sequence,session,tool_use_id,message])?;
        ensure!(
            changed == 1,
            "Provider receipt correlation differs from its original recipient or message"
        );
        Ok(())
    }
}
