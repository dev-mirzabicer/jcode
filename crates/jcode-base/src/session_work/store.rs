//! The private session-work store: one owner-only SQLite database in WAL
//! mode. It fails closed on missing, unreadable or unknown-schema state, and
//! every mutation is idempotent by its request or invocation identity.
use super::{SessionWorkError, workflow};
use chrono::{DateTime, Utc};
use jcode_session_work_types::{
    FinishPolicy, ItemAlias, ItemKind, ModuleStatus, ModuleTiming, RevisionSource, SessionItem,
    SessionWorkActivation, Workflow, WorkflowRevision,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The schema this build reads and writes. Any other version fails closed.
pub const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "store.sqlite3";
const TABLES: &[&str] = &[
    "activation",
    "items",
    "workflow_revisions",
    "workflow_state",
    "module_times",
    "events",
    "journal",
];

const SCHEMA: &str = "
CREATE TABLE activation(
    session TEXT PRIMARY KEY,
    body TEXT NOT NULL
) STRICT;
CREATE TABLE items(
    session TEXT NOT NULL REFERENCES activation(session),
    kind TEXT NOT NULL,
    number INTEGER NOT NULL CHECK(number > 0),
    request TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY(session, kind, number),
    UNIQUE(session, request)
) STRICT;
CREATE TABLE workflow_revisions(
    session TEXT NOT NULL REFERENCES activation(session),
    revision INTEGER NOT NULL CHECK(revision > 0),
    request TEXT NOT NULL,
    text TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(session, revision),
    UNIQUE(session, request)
) STRICT;
CREATE TABLE workflow_state(
    session TEXT PRIMARY KEY REFERENCES activation(session),
    head INTEGER NOT NULL CHECK(head > 0),
    synced INTEGER NOT NULL CHECK(synced >= 0 AND synced <= head)
) STRICT;
CREATE TABLE module_times(
    session TEXT NOT NULL REFERENCES activation(session),
    module TEXT NOT NULL,
    first_active_at TEXT,
    finished_at TEXT,
    finished_as TEXT,
    PRIMARY KEY(session, module)
) STRICT;
-- Attention events and the activity journal are permanent records. Their
-- rows are never removed with a session.
CREATE TABLE events(
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    session TEXT NOT NULL,
    at TEXT NOT NULL,
    level TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL
) STRICT;
CREATE TABLE journal(
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    session TEXT NOT NULL,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL
) STRICT;
";

/// The result of committing one workflow revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub revision: u32,
    /// The request had already committed this revision; nothing changed.
    pub replayed: bool,
}

/// The current workflow of a session and how far its file is known to match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowHead {
    pub revision: WorkflowRevision,
    /// The newest revision written to the file by the host. Lower than the
    /// head only after an interruption between the store commit and the file.
    pub synced: u32,
}

/// A revision installed together with an activation (a template or a copy).
#[derive(Clone, Debug)]
pub struct InitialRevision {
    pub text: String,
    pub source: RevisionSource,
}

#[derive(Clone, Debug)]
pub struct SessionWorkStore {
    root: PathBuf,
}

impl SessionWorkStore {
    /// The store under the durable-state directory.
    pub fn new() -> Self {
        Self::at(crate::storage::durable_state_dir().join("session-work"))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    pub fn exists(&self) -> Result<bool, SessionWorkError> {
        match std::fs::symlink_metadata(self.path()) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(SessionWorkError::corrupt(error)),
        }
    }

    /// Open an existing store. A missing store is an error: only activation
    /// creates it, so a session that expects state must not see it as empty.
    fn open(&self) -> Result<Connection, SessionWorkError> {
        if !self.exists()? {
            return Err(SessionWorkError::StoreMissing(self.path()));
        }
        let connection = raw_connection(&self.path(), false)?;
        check_schema(&connection)?;
        Ok(connection)
    }

    /// Open the store, creating it with the current schema when absent.
    fn open_or_create(&self) -> Result<Connection, SessionWorkError> {
        private_dir(&self.root)?;
        let connection = raw_connection(&self.path(), true)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(SessionWorkError::corrupt)?;
        if version == 0 {
            let tables: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type='table'",
                    [],
                    |row| row.get(0),
                )
                .map_err(SessionWorkError::corrupt)?;
            if tables != 0 {
                return Err(SessionWorkError::UnknownSchema(
                    "tables without a schema version".into(),
                ));
            }
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(SessionWorkError::io)?;
            let mut connection = connection;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Exclusive)
                .map_err(SessionWorkError::io)?;
            let version: u32 = transaction
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .map_err(SessionWorkError::corrupt)?;
            if version == 0 {
                transaction
                    .execute_batch(SCHEMA)
                    .map_err(SessionWorkError::io)?;
                transaction
                    .pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(SessionWorkError::io)?;
            }
            transaction.commit().map_err(SessionWorkError::io)?;
            check_schema(&connection)?;
            return Ok(connection);
        }
        check_schema(&connection)?;
        Ok(connection)
    }

    /// Record a session's activation, with an optional revision 1. Repeating
    /// the identical activation converges; a different one is a conflict.
    pub fn activate(
        &self,
        activation: &SessionWorkActivation,
        initial: Option<&InitialRevision>,
    ) -> Result<(), SessionWorkError> {
        let mut connection = self.open_or_create()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        let body = encode(activation)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM activation WHERE session=?1",
                [&activation.session],
                |row| row.get(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some(existing) = existing {
            let existing: SessionWorkActivation = decode(&existing)?;
            if existing != *activation {
                return Err(SessionWorkError::Conflict(format!(
                    "session {} is already activated differently",
                    activation.session
                )));
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO activation(session, body) VALUES(?1, ?2)",
                params![activation.session, body],
            )
            .map_err(SessionWorkError::io)?;
        if let Some(initial) = initial {
            let parsed = workflow::parse_workflow(&initial.text)
                .map_err(SessionWorkError::InvalidWorkflow)?;
            let baseline = match initial.source {
                RevisionSource::Transfer { .. } => Baseline::Copied,
                _ => Baseline::Empty,
            };
            insert_revision(
                &transaction,
                &activation.session,
                1,
                "activation",
                &initial.text,
                &initial.source,
                activation.activated_at,
            )?;
            record_module_times(
                &transaction,
                &activation.session,
                None,
                &parsed,
                baseline,
                activation.activated_at,
            )?;
            transaction
                .execute(
                    "INSERT INTO workflow_state(session, head, synced) VALUES(?1, 1, 0)",
                    [&activation.session],
                )
                .map_err(SessionWorkError::io)?;
        }
        transaction.commit().map_err(SessionWorkError::io)
    }

    pub fn activation(
        &self,
        session: &str,
    ) -> Result<Option<SessionWorkActivation>, SessionWorkError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT body FROM activation WHERE session=?1",
                [session],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?
            .map(|body| decode(&body))
            .transpose()
    }

    /// Validate and commit one workflow text. The same request with the same
    /// text converges on its original revision; with other text it conflicts.
    pub fn commit_workflow(
        &self,
        session: &str,
        request: &str,
        text: &str,
        source: &RevisionSource,
        now: DateTime<Utc>,
    ) -> Result<CommitReceipt, SessionWorkError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let replay: Option<(u32, String)> = transaction
            .query_row(
                "SELECT revision, text FROM workflow_revisions WHERE session=?1 AND request=?2",
                params![session, request],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some((revision, committed)) = replay {
            if committed != text {
                return Err(SessionWorkError::Conflict(format!(
                    "request {request} already committed different workflow text as revision {revision}"
                )));
            }
            return Ok(CommitReceipt {
                revision,
                replayed: true,
            });
        }
        let parsed = workflow::parse_workflow(text).map_err(SessionWorkError::InvalidWorkflow)?;
        let head = read_head(&transaction, session)?;
        let previous = match &head {
            Some(head) => Some(workflow::parse_workflow(&head.revision.text).map_err(
                |errors| {
                    SessionWorkError::Corrupt(format!(
                        "stored revision {} no longer parses: {errors}",
                        head.revision.revision
                    ))
                },
            )?),
            None => None,
        };
        let revision = head.as_ref().map_or(1, |head| head.revision.revision + 1);
        insert_revision(&transaction, session, revision, request, text, source, now)?;
        record_module_times(
            &transaction,
            session,
            previous.as_ref(),
            &parsed,
            Baseline::Empty,
            now,
        )?;
        match head {
            Some(_) => transaction.execute(
                "UPDATE workflow_state SET head=?2 WHERE session=?1",
                params![session, revision],
            ),
            None => transaction.execute(
                "INSERT INTO workflow_state(session, head, synced) VALUES(?1, ?2, 0)",
                params![session, revision],
            ),
        }
        .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(CommitReceipt {
            revision,
            replayed: false,
        })
    }

    /// Record that the file now holds `revision`. Never moves backwards.
    pub fn mark_synced(&self, session: &str, revision: u32) -> Result<(), SessionWorkError> {
        let connection = self.open()?;
        let changed = connection
            .execute(
                "UPDATE workflow_state SET synced=max(synced, ?2) WHERE session=?1 AND head>=?2",
                params![session, revision],
            )
            .map_err(SessionWorkError::io)?;
        if changed != 1 {
            return Err(SessionWorkError::Corrupt(format!(
                "session {session} has no workflow revision {revision} to mark written"
            )));
        }
        Ok(())
    }

    pub fn workflow_head(&self, session: &str) -> Result<Option<WorkflowHead>, SessionWorkError> {
        let connection = self.open()?;
        read_head(&connection, session)
    }

    /// The newest `limit` revisions, newest first.
    pub fn recent_revisions(
        &self,
        session: &str,
        limit: usize,
    ) -> Result<Vec<WorkflowRevision>, SessionWorkError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT revision, text, source, created_at FROM workflow_revisions
                 WHERE session=?1 ORDER BY revision DESC LIMIT ?2",
            )
            .map_err(SessionWorkError::corrupt)?;
        let rows = statement
            .query_map(params![session, limit as i64], revision_row)
            .map_err(SessionWorkError::corrupt)?;
        rows.map(|row| row.map_err(SessionWorkError::corrupt)?)
            .collect()
    }

    pub fn module_times(&self, session: &str) -> Result<Vec<ModuleTiming>, SessionWorkError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT module, first_active_at, finished_at, finished_as FROM module_times
                 WHERE session=?1 ORDER BY module",
            )
            .map_err(SessionWorkError::corrupt)?;
        let rows = statement
            .query_map([session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(SessionWorkError::corrupt)?;
        rows.map(|row| {
            let (module, first, finished, finished_as) = row.map_err(SessionWorkError::corrupt)?;
            Ok(ModuleTiming {
                module: jcode_session_work_types::ModuleId::parse(&module)
                    .map_err(SessionWorkError::Corrupt)?,
                first_active_at: first.as_deref().map(parse_time).transpose()?,
                finished_at: finished.as_deref().map(parse_time).transpose()?,
                finished_as: finished_as.as_deref().map(parse_status).transpose()?,
            })
        })
        .collect()
    }

    /// Allocate the next alias of `kind` for `session`. The same request
    /// returns its original item.
    pub fn allocate_item(
        &self,
        session: &str,
        request: &str,
        kind: ItemKind,
        label: &str,
        policy: FinishPolicy,
        now: DateTime<Utc>,
    ) -> Result<SessionItem, SessionWorkError> {
        if !kind.allocates_numbers() {
            return Err(SessionWorkError::Invalid(format!(
                "{kind:?} items are named by their proposal and are not allocated"
            )));
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        require_activation(&transaction, session)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM items WHERE session=?1 AND request=?2",
                params![session, request],
                |row| row.get(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?;
        if let Some(body) = existing {
            let item: SessionItem = decode(&body)?;
            if item.alias.kind() != kind || item.label != label || item.policy != policy {
                return Err(SessionWorkError::Conflict(format!(
                    "request {request} already allocated {} differently",
                    item.alias
                )));
            }
            return Ok(item);
        }
        let kind_key = encode_plain(&kind)?;
        let number: u32 = transaction
            .query_row(
                "SELECT coalesce(max(number), 0) + 1 FROM items WHERE session=?1 AND kind=?2",
                params![session, kind_key],
                |row| row.get(0),
            )
            .map_err(SessionWorkError::corrupt)?;
        let item = SessionItem {
            alias: ItemAlias::new(kind, number)
                .map_err(|error| SessionWorkError::Corrupt(error.to_string()))?,
            id: uuid::Uuid::new_v4(),
            label: label.to_string(),
            policy,
            created_at: now,
            finished_at: None,
        };
        transaction
            .execute(
                "INSERT INTO items(session, kind, number, request, body) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![session, kind_key, number, request, encode(&item)?],
            )
            .map_err(SessionWorkError::io)?;
        transaction.commit().map_err(SessionWorkError::io)?;
        Ok(item)
    }

    pub fn item(
        &self,
        session: &str,
        alias: ItemAlias,
    ) -> Result<Option<SessionItem>, SessionWorkError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT body FROM items WHERE session=?1 AND kind=?2 AND number=?3",
                params![session, encode_plain(&alias.kind())?, alias.number()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(SessionWorkError::corrupt)?
            .map(|body| decode(&body))
            .transpose()
    }

    /// Append a permanent journal row. Returns its global sequence.
    pub fn append_journal(
        &self,
        session: &str,
        kind: &str,
        body: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<i64, SessionWorkError> {
        let connection = self.open()?;
        connection
            .execute(
                "INSERT INTO journal(session, at, kind, body) VALUES(?1, ?2, ?3, ?4)",
                params![session, now.to_rfc3339(), kind, body.to_string()],
            )
            .map_err(SessionWorkError::io)?;
        Ok(connection.last_insert_rowid())
    }

    /// Append an attention event. Returns its global sequence, the cursor
    /// followers resume from.
    pub fn append_event(
        &self,
        session: &str,
        level: &str,
        kind: &str,
        body: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<i64, SessionWorkError> {
        let connection = self.open()?;
        connection
            .execute(
                "INSERT INTO events(session, at, level, kind, body) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![session, now.to_rfc3339(), level, kind, body.to_string()],
            )
            .map_err(SessionWorkError::io)?;
        Ok(connection.last_insert_rowid())
    }

    /// Events after `cursor`, oldest first: `(sequence, session, kind)`.
    pub fn events_after(
        &self,
        cursor: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String, String)>, SessionWorkError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, session, kind FROM events WHERE sequence>?1
                 ORDER BY sequence LIMIT ?2",
            )
            .map_err(SessionWorkError::corrupt)?;
        let rows = statement
            .query_map(params![cursor, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(SessionWorkError::corrupt)?;
        rows.map(|row| row.map_err(SessionWorkError::corrupt))
            .collect()
    }

    /// Remove the state of a session that was never published. Permanent
    /// journal and event rows are kept. A missing store has nothing to remove.
    pub fn remove_unpublished(&self, session: &str) -> Result<(), SessionWorkError> {
        if !self.exists()? {
            return Ok(());
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SessionWorkError::io)?;
        for table in [
            "items",
            "workflow_revisions",
            "workflow_state",
            "module_times",
            "activation",
        ] {
            transaction
                .execute(&format!("DELETE FROM {table} WHERE session=?1"), [session])
                .map_err(SessionWorkError::io)?;
        }
        transaction.commit().map_err(SessionWorkError::io)
    }
}

impl Default for SessionWorkStore {
    fn default() -> Self {
        Self::new()
    }
}

fn require_activation(connection: &Connection, session: &str) -> Result<(), SessionWorkError> {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM activation WHERE session=?1)",
            [session],
            |row| row.get(0),
        )
        .map_err(SessionWorkError::corrupt)?;
    if present {
        Ok(())
    } else {
        Err(SessionWorkError::NotActivated(session.to_string()))
    }
}

fn read_head(
    connection: &Connection,
    session: &str,
) -> Result<Option<WorkflowHead>, SessionWorkError> {
    let state: Option<(u32, u32)> = connection
        .query_row(
            "SELECT head, synced FROM workflow_state WHERE session=?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?;
    let Some((head, synced)) = state else {
        return Ok(None);
    };
    let revision = connection
        .query_row(
            "SELECT revision, text, source, created_at FROM workflow_revisions
             WHERE session=?1 AND revision=?2",
            params![session, head],
            revision_row,
        )
        .optional()
        .map_err(SessionWorkError::corrupt)?
        .ok_or_else(|| {
            SessionWorkError::Corrupt(format!(
                "session {session} points at missing workflow revision {head}"
            ))
        })??;
    Ok(Some(WorkflowHead { revision, synced }))
}

type RevisionRow = Result<WorkflowRevision, SessionWorkError>;

fn revision_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RevisionRow> {
    let revision: u32 = row.get(0)?;
    let text: String = row.get(1)?;
    let source: String = row.get(2)?;
    let created_at: String = row.get(3)?;
    Ok((|| {
        Ok(WorkflowRevision {
            revision,
            text,
            source: decode(&source)?,
            created_at: parse_time(&created_at)?,
        })
    })())
}

fn insert_revision(
    connection: &Connection,
    session: &str,
    revision: u32,
    request: &str,
    text: &str,
    source: &RevisionSource,
    now: DateTime<Utc>,
) -> Result<(), SessionWorkError> {
    connection
        .execute(
            "INSERT INTO workflow_revisions(session, revision, request, text, source, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session,
                revision,
                request,
                text,
                encode(source)?,
                now.to_rfc3339()
            ],
        )
        .map_err(SessionWorkError::io)?;
    Ok(())
}

/// How revision 1's modules relate to time already spent.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Baseline {
    /// Every module status starts in this session.
    Empty,
    /// Copied from another session: finished modules were finished there.
    Copied,
}

/// Record module transitions between two revisions. A module becoming active
/// keeps its first activation time; becoming done or skipped records the
/// finish; leaving a finished state clears it.
fn record_module_times(
    connection: &Connection,
    session: &str,
    previous: Option<&Workflow>,
    next: &Workflow,
    baseline: Baseline,
    now: DateTime<Utc>,
) -> Result<(), SessionWorkError> {
    let now = now.to_rfc3339();
    for (_, module) in next.walk() {
        let before = previous
            .and_then(|workflow| workflow.module(module.id.as_str()))
            .map(|module| module.status);
        let id = module.id.as_str();
        if module.status == ModuleStatus::Active && before != Some(ModuleStatus::Active) {
            connection
                .execute(
                    "INSERT INTO module_times(session, module, first_active_at) VALUES(?1, ?2, ?3)
                     ON CONFLICT(session, module) DO UPDATE SET
                       first_active_at=coalesce(first_active_at, excluded.first_active_at)",
                    params![session, id, now],
                )
                .map_err(SessionWorkError::io)?;
        }
        let finished_now = module.status.is_finished();
        let finished_before = before.is_some_and(ModuleStatus::is_finished);
        if finished_now && before != Some(module.status) {
            if before.is_none() && baseline == Baseline::Copied {
                continue;
            }
            connection
                .execute(
                    "INSERT INTO module_times(session, module, finished_at, finished_as)
                     VALUES(?1, ?2, ?3, ?4)
                     ON CONFLICT(session, module) DO UPDATE SET
                       finished_at=excluded.finished_at, finished_as=excluded.finished_as",
                    params![session, id, now, encode_plain(&module.status)?],
                )
                .map_err(SessionWorkError::io)?;
        } else if !finished_now && finished_before {
            connection
                .execute(
                    "UPDATE module_times SET finished_at=NULL, finished_as=NULL
                     WHERE session=?1 AND module=?2",
                    params![session, id],
                )
                .map_err(SessionWorkError::io)?;
        }
    }
    Ok(())
}

fn check_schema(connection: &Connection) -> Result<(), SessionWorkError> {
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(SessionWorkError::corrupt)?;
    if version != SCHEMA_VERSION {
        return Err(SessionWorkError::UnknownSchema(format!(
            "schema version {version}, this build reads {SCHEMA_VERSION}"
        )));
    }
    for table in TABLES {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(SessionWorkError::corrupt)?;
        if !present {
            return Err(SessionWorkError::UnknownSchema(format!(
                "table {table} is missing"
            )));
        }
    }
    Ok(())
}

fn raw_connection(path: &Path, create: bool) -> Result<Connection, SessionWorkError> {
    // SQLITE_OPEN_NOFOLLOW rejects a symlinked leaf; resolve only the parent.
    let parent = path
        .parent()
        .ok_or_else(|| SessionWorkError::Io("store has no parent directory".into()))?
        .canonicalize()
        .map_err(SessionWorkError::io)?;
    let path = parent.join(path.file_name().unwrap_or_default());
    let mut flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if create {
        flags |= rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
    }
    let connection =
        Connection::open_with_flags(&path, flags).map_err(SessionWorkError::corrupt)?;
    jcode_core::fs::set_permissions_owner_only(&path).map_err(SessionWorkError::io)?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error)?;
    // Reading the schema proves the file is a database before any decision.
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(SessionWorkError::corrupt)?;
    Ok(connection)
}

pub(super) fn private_dir(path: &Path) -> Result<(), SessionWorkError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(SessionWorkError::Corrupt(format!(
                    "{} is not a private directory",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(SessionWorkError::io)?;
        }
        Err(error) => return Err(SessionWorkError::io(error)),
    }
    jcode_core::fs::set_directory_permissions_owner_only(path).map_err(SessionWorkError::io)
}

/// A file that is not a database, or a damaged one, is corrupt state; other
/// failures (busy, I/O) are retryable.
fn sqlite_error(error: rusqlite::Error) -> SessionWorkError {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SessionWorkError::corrupt(error)
        }
        _ => SessionWorkError::io(error),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, SessionWorkError> {
    serde_json::to_string(value).map_err(|error| SessionWorkError::Io(error.to_string()))
}

/// A unit enum's bare serialized name, such as `task` or `done`.
fn encode_plain<T: serde::Serialize>(value: &T) -> Result<String, SessionWorkError> {
    match serde_json::to_value(value).map_err(|error| SessionWorkError::Io(error.to_string()))? {
        serde_json::Value::String(text) => Ok(text),
        other => Err(SessionWorkError::Io(format!("{other} is not a plain name"))),
    }
}

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, SessionWorkError> {
    serde_json::from_str(text).map_err(SessionWorkError::corrupt)
}

fn parse_time(text: &str) -> Result<DateTime<Utc>, SessionWorkError> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(SessionWorkError::corrupt)
}

fn parse_status(text: &str) -> Result<ModuleStatus, SessionWorkError> {
    serde_json::from_value(serde_json::Value::String(text.to_string()))
        .map_err(SessionWorkError::corrupt)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
