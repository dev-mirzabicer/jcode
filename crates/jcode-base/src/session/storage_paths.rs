use anyhow::Result;
use std::path::{Path, PathBuf};

use super::PersistVectorMode;
use crate::storage;

pub(crate) fn session_path_in_dir(base: &std::path::Path, session_id: &str) -> PathBuf {
    base.join("sessions").join(format!("{}.json", session_id))
}

pub(super) use crate::process_memory::estimate_json_bytes;

pub(super) fn file_len_or_zero(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

pub(super) fn persist_vector_mode_label(mode: PersistVectorMode) -> &'static str {
    match mode {
        PersistVectorMode::Clean => "clean",
        PersistVectorMode::Append => "append",
        PersistVectorMode::Full => "full",
    }
}

pub fn session_path(session_id: &str) -> Result<PathBuf> {
    let base = storage::jcode_dir()?;
    Ok(session_path_in_dir(&base, session_id))
}

pub fn session_journal_path_from_snapshot(path: &Path) -> PathBuf {
    let mut name = path
        .file_stem()
        .map(|stem| stem.to_os_string())
        .unwrap_or_default();
    name.push(".journal.jsonl");
    path.with_file_name(name)
}

pub fn session_journal_path(session_id: &str) -> Result<PathBuf> {
    Ok(session_journal_path_from_snapshot(&session_path(
        session_id,
    )?))
}

pub fn session_exists(session_id: &str) -> bool {
    session_path(session_id)
        .map(|path| path.exists())
        .unwrap_or(false)
}

/// Remove the private artifacts of a session that failed before publication.
///
/// Ordinary user deletion is intentionally not exposed through this function.
/// Callers use it only while they still exclusively own a newly-created session
/// that never became usable.
pub fn remove_unpublished_session(session_id: &str) -> Result<()> {
    storage::unregister_active_pid(session_id);
    let snapshot = session_path(session_id)?;
    let journal = session_journal_path_from_snapshot(&snapshot);
    for path in [
        snapshot.clone(),
        snapshot.with_extension("bak"),
        journal.clone(),
        journal.with_extension("bak"),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error.into());
            }
        }
    }
    crate::todo::remove_todos(session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpublished_session_cleanup_removes_snapshot_journal_backup_todo_and_presence() {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("test home");
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        let id = "session-unpublished-cleanup";
        let mut session = crate::session::Session::create_with_id(id.to_string(), None, None);
        session.ensure_initial_session_context_message();
        session.mark_active();
        session.save().expect("save unpublished session");

        let snapshot = session_path(id).expect("snapshot path");
        let journal = session_journal_path_from_snapshot(&snapshot);
        std::fs::write(snapshot.with_extension("bak"), b"snapshot backup")
            .expect("write snapshot backup");
        std::fs::write(&journal, b"journal").expect("write journal");
        std::fs::write(journal.with_extension("bak"), b"journal backup")
            .expect("write journal backup");
        let todo = home.path().join("todos").join(format!("{id}.json"));
        std::fs::create_dir_all(todo.parent().unwrap()).expect("create todo dir");
        std::fs::write(&todo, b"[]").expect("write todo sidecar");

        remove_unpublished_session(id).expect("remove unpublished session");
        assert!(!snapshot.exists());
        assert!(!snapshot.with_extension("bak").exists());
        assert!(!journal.exists());
        assert!(!journal.with_extension("bak").exists());
        assert!(!todo.exists());
        assert!(
            !crate::session::active_session_ids()
                .iter()
                .any(|active| active == id)
        );

        match previous {
            Some(value) => crate::env::set_var("JCODE_HOME", value),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }
}
