//! Per-client recovery of unsent intent. This never writes an instruction store
//! or restores a queued source action automatically.
use super::*;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) enum LocalRecoveryRequest {
    List,
    Load(String),
    Archive,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LocalSnapshot {
    schema: u32,
    pub session: String,
    client: String,
    pub title: String,
    draft: Option<(InstructionEditScope, String, u64, String)>,
    form: Option<EditForm>,
    pending: Option<InstructionManagementRequest>,
    operation: Option<String>,
    filter: InstructionFilter,
    target: Option<InstructionInspectionTarget>,
    snapshot: Option<String>,
}
impl LocalSnapshot {
    pub fn capture(manager: &InstructionManager, client: &str) -> Option<Self> {
        let ui = &manager.editing;
        let pending = ui.suspended_request.clone().or_else(|| {
            ui.pending
                .as_ref()
                .map(|(_, _, request)| request.clone())
                .or(ui.queued.clone())
        });
        let pending = pending.filter(|request| {
            !matches!(
                request,
                InstructionManagementRequest::Recoveries
                    | InstructionManagementRequest::RepositoryChoices { .. }
                    | InstructionManagementRequest::RepositoryReceipt { .. }
                    | InstructionManagementRequest::CompareDraft { .. }
                    | InstructionManagementRequest::Review { .. }
                    | InstructionManagementRequest::Resume { .. }
            )
        });
        let form = ui.form.clone().or(ui.submitted_form.clone()).or_else(|| {
            ui.local_loaded
                .as_ref()
                .and_then(|snapshot| snapshot.form.clone())
        });
        if ui.draft.is_none() && form.is_none() && pending.is_none() && ui.repository_plan.is_none()
        {
            return None;
        }
        let draft = ui
            .draft
            .as_ref()
            .map(|draft| {
                (
                    draft.scope,
                    draft.id.clone(),
                    draft.generation,
                    draft
                        .files
                        .get(ui.file_index)
                        .map(|file| file.key.clone())
                        .unwrap_or_default(),
                )
            })
            .or_else(|| {
                ui.local_loaded
                    .as_ref()
                    .and_then(|snapshot| snapshot.draft.clone())
            });
        Some(Self {
            schema: 1,
            session: manager.session.clone(),
            client: client.into(),
            title: ui
                .draft
                .as_ref()
                .map(|draft| draft.title.clone())
                .or_else(|| form.as_ref().map(|form| form.title.clone()))
                .unwrap_or_else(|| "Repository operation recovery".into()),
            draft,
            form,
            pending,
            operation: ui
                .repository_plan
                .as_ref()
                .map(|plan| plan.id.clone())
                .or_else(|| {
                    ui.local_loaded
                        .as_ref()
                        .and_then(|snapshot| snapshot.operation.clone())
                }),
            filter: manager.filter.clone(),
            target: manager.selected_target(),
            snapshot: manager.snapshot_id(),
        })
    }
    pub fn draft_reference(&self) -> Option<(InstructionEditScope, String)> {
        self.draft
            .as_ref()
            .map(|(scope, id, _, _)| (*scope, id.clone()))
    }
}

#[derive(Clone)]
pub(crate) struct LocalRecoveryRow {
    pub key: String,
    pub title: String,
    pub active_elsewhere: bool,
    pub error: Option<String>,
}
#[derive(Clone)]
struct WriteJob {
    generation: u64,
    session: String,
    snapshot: Option<LocalSnapshot>,
}
#[derive(Default)]
struct Queue {
    generation: u64,
    pending: BTreeMap<String, WriteJob>,
    running: bool,
    durable: BTreeMap<String, u64>,
    errors: BTreeMap<String, String>,
    reported: BTreeMap<String, u64>,
}
struct Owner {
    file: Option<File>,
    attempted: u64,
}
#[derive(Clone)]
pub(crate) struct RecoveryStore {
    root: PathBuf,
    client: String,
    queue: Arc<Mutex<Queue>>,
    owners: Arc<Mutex<BTreeMap<String, Owner>>>,
}
impl RecoveryStore {
    pub fn new(root: PathBuf, client: String) -> Self {
        Self {
            root,
            client,
            queue: Arc::new(Mutex::new(Queue::default())),
            owners: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn schedule(&self, session: &str, snapshot: Option<LocalSnapshot>) -> u64 {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.generation = queue.generation.saturating_add(1);
        let generation = queue.generation;
        queue.pending.insert(
            session.into(),
            WriteJob {
                generation,
                session: session.into(),
                snapshot,
            },
        );
        if !queue.running {
            queue.running = true;
            let store = self.clone();
            tokio::task::spawn_blocking(move || store.drain());
        }
        generation
    }
    fn drain(&self) {
        loop {
            let job = {
                let mut queue = self
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match queue.pending.pop_first() {
                    Some((_, job)) => job,
                    None => {
                        queue.running = false;
                        return;
                    }
                }
            };
            self.persist_job(job);
        }
    }
    fn persist_job(&self, job: WriteJob) {
        let result = self.write(&job);
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue
            .reported
            .get(&job.session)
            .is_some_and(|reported| *reported > job.generation)
        {
            return;
        }
        queue.reported.insert(job.session.clone(), job.generation);
        match result {
            Ok(()) => {
                let done = queue.durable.entry(job.session.clone()).or_default();
                *done = (*done).max(job.generation);
                queue.errors.remove(&job.session);
            }
            Err(error) => {
                queue.errors.insert(job.session, format!("{error:#}"));
            }
        }
    }
    pub fn ready(&self, session: &str, generation: u64) -> Result<bool> {
        let queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = queue.errors.get(session)
            && queue.reported.get(session).copied().unwrap_or(0) >= generation
        {
            anyhow::bail!(
                "Local recovery could not be preserved: {error}. Keep this client open, repair storage, and retry. No new source action was dispatched."
            );
        }
        Ok(queue.durable.get(session).copied().unwrap_or(0) >= generation)
    }
    /// Orderly reload flushes the final intent without depending on event ticks.
    pub fn flush(&self, session: &str, snapshot: Option<LocalSnapshot>) -> Result<()> {
        let generation = {
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.generation = queue.generation.saturating_add(1);
            queue.generation
        };
        self.persist_job(WriteJob {
            generation,
            session: session.into(),
            snapshot,
        });
        self.ready(session, generation).and_then(|ready| {
            if ready {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Local recovery flush did not complete"))
            }
        })
    }
    fn directory(&self, session: &str, create: bool) -> Result<PathBuf> {
        let mut path = self.root.clone();
        if create {
            crate::storage::ensure_dir(&path)?;
        }
        for part in ["instruction-editor", "client-recovery", &digest(session)] {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => anyhow::ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "Recovery directory is not a real directory: {}",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                    std::fs::create_dir(&path)?;
                    crate::platform::set_directory_permissions_owner_only(&path)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(path)
    }
    fn write(&self, job: &WriteJob) -> Result<()> {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = owners.entry(job.session.clone()).or_insert(Owner {
            file: None,
            attempted: 0,
        });
        if owner.attempted >= job.generation {
            return Ok(());
        }
        owner.attempted = job.generation;
        let directory = self.directory(&job.session, job.snapshot.is_some())?;
        let key = digest(&self.client);
        let path = directory.join(format!("{key}.json"));
        if job.snapshot.is_none() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            owner.file = None;
            return Ok(());
        }
        if owner.file.is_none() {
            let file = open_regular(&directory.join(format!("{key}.lock")), true)?;
            file.try_lock().context("Own local instruction recovery")?;
            owner.file = Some(file);
        }
        anyhow::ensure!(
            !path.is_symlink(),
            "Recovery file is a symlink: {}",
            path.display()
        );
        crate::storage::write_json_secret(
            &path,
            job.snapshot.as_ref().context("Missing local intent")?,
        )?;

        Ok(())
    }
    pub fn archive(&self, snapshot: &LocalSnapshot) -> Result<String> {
        let mut archived = snapshot.clone();
        archived.client = format!("archive-{}", digest(&serde_json::to_string(snapshot)?));
        let directory = self.directory(&snapshot.session, true)?;
        let key = digest(&archived.client);
        let lock = open_regular(&directory.join(format!("{key}.lock")), true)?;
        lock.try_lock().context("Preserve archived local intent")?;
        let path = directory.join(format!("{key}.json"));
        if path.exists() || path.is_symlink() {
            let existing: serde_json::Value =
                serde_json::from_reader(std::io::BufReader::new(open_regular(&path, false)?))?;
            anyhow::ensure!(
                existing == serde_json::to_value(&archived)?,
                "Archived intent differs; the existing record was preserved"
            );
        } else {
            crate::storage::write_json_secret(&path, &archived)?;
        }
        Ok(key)
    }

    pub fn list(&self, session: &str) -> Result<Vec<LocalRecoveryRow>> {
        let directory = self.directory(session, false)?;
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
            session: String,
            client: String,
            title: String,
        }
        let mut rows = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let Some(key) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|key| valid_key(key))
            else {
                continue;
            };
            if path.extension().is_none_or(|value| value != "json") {
                continue;
            }
            let header = (|| -> Result<Header> {
                let header: Header =
                    serde_json::from_reader(std::io::BufReader::new(open_regular(&path, false)?))?;
                anyhow::ensure!(
                    header.schema == 1
                        && header.session == session
                        && digest(&header.client) == key,
                    "Recovery identity is inconsistent"
                );
                Ok(header)
            })();
            let header = match header {
                Ok(header) => header,
                Err(error) => {
                    rows.push(LocalRecoveryRow {
                        key: key.into(),
                        title: "Local recovery record needs inspection".into(),
                        active_elsewhere: false,
                        error: Some(format!(
                            "{}: {error:#}. The record was retained.",
                            path.display()
                        )),
                    });
                    continue;
                }
            };
            let active_elsewhere = if header.client == self.client {
                false
            } else {
                match open_regular(&directory.join(format!("{key}.lock")), false)?.try_lock() {
                    Ok(()) => false,
                    Err(std::fs::TryLockError::WouldBlock) => true,
                    Err(error) => {
                        return Err(anyhow::anyhow!(
                            "Cannot inspect local recovery ownership: {error}"
                        ));
                    }
                }
            };
            rows.push(LocalRecoveryRow {
                key: key.into(),
                title: header.title,
                active_elsewhere,
                error: None,
            });
        }
        rows.sort_by(|left, right| left.title.cmp(&right.title).then(left.key.cmp(&right.key)));
        Ok(rows)
    }
    pub fn load(&self, session: &str, key: &str) -> Result<LocalSnapshot> {
        anyhow::ensure!(valid_key(key), "Invalid local recovery identity");
        let directory = self.directory(session, false)?;
        let owner = open_regular(&directory.join(format!("{key}.lock")), false)?;
        if key != digest(&self.client) {
            owner
                .try_lock()
                .context("Another client still owns this unsent intent")?;
        }
        let mut snapshot: LocalSnapshot = serde_json::from_reader(std::io::BufReader::new(
            open_regular(&directory.join(format!("{key}.json")), false)?,
        ))?;
        anyhow::ensure!(
            snapshot.schema == 1 && snapshot.session == session && digest(&snapshot.client) == key,
            "Local recovery belongs to another session or has an unsupported schema"
        );
        if let Some(form) = &mut snapshot.form {
            form.repair_cursors();
        }
        Ok(snapshot)
    }
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|value| value.is_ascii_hexdigit())
}
fn open_regular(path: &Path, create: bool) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(
            create
                || path
                    .extension()
                    .is_some_and(|extension| extension == "lock"),
        )
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("Open recovery file {}", path.display()))?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "Recovery path is not a regular file: {}",
        path.display()
    );
    if create {
        crate::platform::set_permissions_owner_only(path)?;
    }
    Ok(file)
}

impl InstructionManager {
    pub(super) fn form_anchor(&self) -> forms::FormAnchor {
        if let Some(draft) = &self.editing.draft {
            forms::FormAnchor {
                revision: None,
                snapshot: None,
                target: None,
                draft: Some((
                    draft.id.clone(),
                    draft.generation,
                    draft
                        .files
                        .get(self.editing.file_index)
                        .map(|file| file.key.clone())
                        .unwrap_or_default(),
                )),
            }
        } else {
            forms::FormAnchor {
                revision: self
                    .revision_selection
                    .as_ref()
                    .map(|value| value.from.clone())
                    .or_else(|| {
                        self.history_visible
                            .then(|| {
                                self.history
                                    .get(self.history_selected)
                                    .map(|entry| entry.commit.clone())
                            })
                            .flatten()
                    }),
                snapshot: self.snapshot_id(),
                target: self.selected_target(),
                draft: None,
            }
        }
    }
    pub(crate) fn open_local_recovery_menu(&mut self, rows: Vec<LocalRecoveryRow>) {
        use super::super::menu::{Menu, MenuAction, MenuItem};
        let mut items = rows.into_iter().map(|row| MenuItem { label: row.title, hint: format!("Local recovery {}. Restore unsent values without executing saved source actions.", row.key), key: "Enter".into(), action: MenuAction::Recovery(RecoveryChoice::Local(row.key)), disabled: if let Some(error) = row.error { Some(error) } else if row.active_elsewhere { Some("Another live client owns these unsent values. Close that client before recovery.".into()) } else if self.editing.draft.is_some() { Some("Close the current draft before restoring local intent.".into()) } else { None } }).collect::<Vec<_>>();
        if items.is_empty() {
            items.push(MenuItem { label: "No retained local changes".into(), hint: "Server-side saved drafts are listed separately under Recover drafts and operations.".into(), key: "Esc".into(), action: MenuAction::Key(KeyCode::Esc), disabled: None });
        }
        self.menu = Some(Menu {
            title: "Local unsent changes".into(),
            items,
            query: String::new(),
            selected: 0,
            context: self.selected_target(),
            snapshot: self.snapshot_id(),
            revision: None,
            parent: None,
            explanation: false,
            explanation_scroll: 0,
        });
    }
    pub(crate) fn restore_local_intent(&mut self, snapshot: LocalSnapshot) {
        if snapshot.session != self.session {
            self.editing.status = "Local recovery belongs to another session.".into();
            return;
        }
        if self.editing.draft.is_some() {
            self.editing.status = "Close the active draft before restoring local intent.".into();
            return;
        }
        self.editing.recovery_dirty = true;
        self.editing.visible = true;
        self.editing.suspended_request = snapshot.pending.clone();
        let reference = snapshot.draft_reference();
        self.editing.local_loaded = Some(snapshot.clone());
        if let Some((scope, draft)) = reference {
            self.editing.resume_needed = true;
            self.editing.queued = Some(InstructionManagementRequest::Resume { scope, draft });
        } else if let Some(form) = snapshot.form {
            self.editing.form = Some(form);
            self.editing.status = "Unsent form restored. Review its current target before submission. Nothing was executed.".into();
        } else if let Some(operation_id) = snapshot.operation {
            self.editing.queued =
                Some(InstructionManagementRequest::RepositoryReceipt { operation_id });
        } else {
            self.editing.status = "Unconfirmed intent retained. Review local values or recover the matching server draft. No action was replayed.".into();
        }
    }
    pub(super) fn complete_local_resume(&mut self) {
        self.editing.resume_needed = false;
        if let Some(draft) = &mut self.editing.draft {
            draft.reviewed = false;
        }
        let Some(snapshot) = &self.editing.local_loaded else {
            return;
        };
        if let Some((_, id, _, file)) = &snapshot.draft
            && let Some(current) = &self.editing.draft
        {
            if &current.id != id {
                return;
            }
            if let Some(index) = current.files.iter().position(|value| &value.key == file) {
                self.editing.file_index = index;
            }
        }
        self.editing.form = snapshot.form.clone();
        self.editing.suspended_request = snapshot.pending.clone();
        self.editing.status = "Recovered draft and local values. No source action was replayed. Review retained values and the current source before Save.".into();
        self.editing.wrapped.clear();
    }
    pub(super) fn review_local_values(&mut self) {
        if self.editing.resume_needed {
            self.editing.status =
                "Recover the server draft before rebinding retained values.".into();
            return;
        }
        let anchor = self.form_anchor();
        self.editing.local_review_anchor = Some(anchor.clone());
        if self.editing.form.is_some() {
            self.editing.submitted_form = self.editing.form.take();
        }
        self.editing.document = format!(
            "REVIEW RETAINED VALUES\n\nCurrent target: {anchor:?}\n\nThis confirmation changes only private draft/form state. It does not Save, push or replay a repository action. A later reviewed Save remains required.\n"
        );
        if let Some(draft) = &self.editing.draft
            && let Some(file) = draft.files.get(self.editing.file_index)
        {
            self.editing.document.push_str(&format!(
                "\nCURRENT SERVER DRAFT\nRepository: {}\nFile: {}\nBody:\n{}\n",
                draft.repository, file.path, file.body
            ));
            let current = EditForm::metadata(file, &draft.choices);
            for field in &current.fields {
                self.editing
                    .document
                    .push_str(&format!("\n{}\n{}\n", field.label, field.value));
            }
        }
        if let Some(form) = &self.editing.submitted_form {
            self.editing.document.push_str(&format!(
                "\nOriginal form target: {:?}\n\n{}\n",
                form.anchor, form.title
            ));
            for field in &form.fields {
                self.editing
                    .document
                    .push_str(&format!("\n{}\n{}\n", field.label, field.value));
            }
        } else if let Some(request) = &self.editing.suspended_request {
            match request {
                InstructionManagementRequest::Update { change, .. } => {
                    if let Some(draft) = &self.editing.draft { self.editing.document.push_str(&format!("\nRepository: {}\nCurrent draft generation: {}\n", draft.repository, draft.generation)); }
                    self.editing.document.push_str(&format!("\nRetained private update:\n{}", serde_json::to_string_pretty(change).unwrap_or_else(|error| error.to_string())));
                }
                _ => self.editing.document.push_str("\nA saved source/repository action is not replayed here. Use the server's retained draft or operation receipt to inspect its outcome."),
            }
        }
        self.editing.confirm = Some(EditAction::ReviewLocalValues);
        self.editing.visible = true;
        self.editing.scroll = 0;
        self.editing.wrapped.clear();
    }
    pub(super) fn apply_local_values(&mut self) {
        let anchor = self.form_anchor();
        if self.editing.local_review_anchor.as_ref() != Some(&anchor) {
            self.editing.status = "Target changed during local-value review. Compare again.".into();
            return;
        }
        if let Some(mut form) = self.editing.submitted_form.take() {
            form.anchor = Some(anchor);
            self.editing.form = Some(form);
            self.editing.suspended_request = None;
            self.editing.local_loaded = None;
            self.editing.status = "Retained form rebound to the compared target. Submit its values, then Review before Save.".into();
        } else if let Some(InstructionManagementRequest::Update { change, .. }) =
            self.editing.suspended_request.clone()
        {
            if let Some(draft) = &self.editing.draft {
                let key = match &change {
                    InstructionDraftChange::Body { file, .. }
                    | InstructionDraftChange::Metadata { file, .. }
                    | InstructionDraftChange::RepairSource { file, .. } => file,
                };
                if !draft
                    .files
                    .iter()
                    .any(|file| &file.key == key && !file.deleted)
                {
                    self.editing.status = "The retained update's target no longer exists. Its complete values remain preserved.".into();
                    return;
                }
                self.editing.queued = Some(InstructionManagementRequest::Update {
                    draft: draft.id.clone(),
                    generation: draft.generation,
                    change,
                });
                self.editing.suspended_request = None;
                self.editing.local_loaded = None;
            }
        } else {
            self.editing.status = "No source action replayed. Recover the saved operation receipt or previous Save through its named action.".into();
        }
        self.editing.local_review_anchor = None;
        self.editing.recovery_dirty = true;
    }
    pub(crate) fn suspend_editing_connection(&mut self, client: &str) {
        let snapshot = LocalSnapshot::capture(self, client);
        self.editing.suspended_request = self
            .editing
            .pending
            .take()
            .map(|(_, _, request)| request)
            .or(self.editing.queued.take())
            .or(self.editing.suspended_request.take());
        self.editing.request_preserved = false;
        self.editing.local_loaded = snapshot;
        self.editing.confirm = None;
        if let Some(draft) = &self.editing.draft {
            self.editing.resume_needed = true;
            self.editing.queued = Some(InstructionManagementRequest::Resume {
                scope: draft.scope,
                draft: draft.id.clone(),
            });
        }
        self.editing.recovery_dirty = true;
        self.editing.status = "Connection changed. Local values are retained; no source action is automatically replayed.".into();
    }
}

impl InstructionManager {
    pub(crate) fn archived_local_intent(&mut self, key: String) {
        self.editing.archiving = false;
        self.editing.suspended_request = None;
        self.editing.local_loaded = None;
        self.editing.form = None;
        self.editing.submitted_form = None;
        self.editing.resume_needed = false;
        self.editing.status = format!(
            "Unsent local values preserved in recovery {key}. No source action was replayed."
        );
        self.editing.recovery_dirty = true;
        if self.editing.draft.is_some() {
            self.editing.queued = Some(InstructionManagementRequest::Close);
        } else {
            self.editing.visible = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(client: &str) -> LocalSnapshot {
        let mut manager = InstructionManager::new("session".into(), false);
        manager.queued = None;
        manager.editing.visible = true;
        let mut form = EditForm::start(EditAction::CreateGlobal, None);
        form.fields[0].value = "unsent-id".into();
        form.fields[0].cursor = 3;
        manager.editing.form = Some(form);
        LocalSnapshot::capture(&manager, client).unwrap()
    }
    #[test]
    fn client_capsules_are_partitioned_private_and_kernel_owned() {
        let root = tempfile::tempdir().unwrap();
        let first = RecoveryStore::new(root.path().into(), "client-a".into());
        first.flush("session", Some(snapshot("client-a"))).unwrap();
        let second = RecoveryStore::new(root.path().into(), "client-b".into());
        let rows = second.list("session").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].active_elsewhere);
        assert!(second.load("session", &rows[0].key).is_err());
        second.flush("session", Some(snapshot("client-b"))).unwrap();
        assert_eq!(second.list("session").unwrap().len(), 2);
        drop(first);
        let loaded = second.load("session", &rows[0].key).unwrap();
        let form = loaded.form.as_ref().unwrap();
        assert_eq!(form.fields[0].value, "unsent-id");
        assert_eq!(form.fields[0].cursor, 3);
        assert!(!root.path().join("instructions").exists());
    }
    #[test]
    fn newer_flush_and_deletion_cannot_be_overwritten_by_older_jobs() {
        let root = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(root.path().into(), "client".into());
        store.persist_job(WriteJob {
            generation: 4,
            session: "session".into(),
            snapshot: Some(snapshot("client")),
        });
        store.persist_job(WriteJob {
            generation: 5,
            session: "session".into(),
            snapshot: None,
        });
        store.persist_job(WriteJob {
            generation: 3,
            session: "session".into(),
            snapshot: Some(snapshot("client")),
        });
        assert!(store.list("session").unwrap().is_empty());
        assert!(store.ready("session", 5).unwrap());
    }
    #[test]
    fn archiving_before_close_preserves_old_values_when_new_edit_reuses_the_client() {
        let root = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(root.path().into(), "client".into());
        let original = snapshot("client");
        store.flush("session", Some(original.clone())).unwrap();
        let key = store.archive(&original).unwrap();
        assert_eq!(store.archive(&original).unwrap(), key);
        store.flush("session", None).unwrap();
        let mut next = snapshot("client");
        next.form.as_mut().unwrap().fields[0].value = "new-edit".into();
        store.flush("session", Some(next)).unwrap();
        let recovered = store.load("session", &key).unwrap();
        assert_eq!(recovered.form.unwrap().fields[0].value, "unsent-id");
    }
    #[test]
    fn corrupt_local_capsule_is_visible_without_hiding_valid_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(root.path().into(), "client".into());
        store.flush("session", Some(snapshot("client"))).unwrap();
        let directory = store.directory("session", false).unwrap();
        let key = digest("corrupt-client");
        std::fs::write(directory.join(format!("{key}.json")), "broken").unwrap();
        let rows = store.list("session").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().filter(|row| row.error.is_some()).count(), 1);
    }
    #[test]
    fn recovering_unsent_form_does_not_dispatch_source_actions() {
        let mut manager = InstructionManager::new("session".into(), false);
        manager.queued = None;
        let mut saved = snapshot("old-client");
        saved.pending = Some(InstructionManagementRequest::ApplyRepository {
            operation_id: "unconfirmed-operation".into(),
        });
        manager.restore_local_intent(saved);
        assert!(manager.editing.form.is_some());
        assert!(manager.editing.queued.is_none());
        assert!(matches!(
            manager.editing.suspended_request,
            Some(InstructionManagementRequest::ApplyRepository { .. })
        ));
    }
    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlink_directories_and_records_without_touching_targets() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("instruction-editor")).unwrap();
        let store = RecoveryStore::new(root.path().into(), "client".into());
        assert!(store.flush("session", Some(snapshot("client"))).is_err());
        assert_eq!(outside.path().read_dir().unwrap().count(), 0);
    }
}
