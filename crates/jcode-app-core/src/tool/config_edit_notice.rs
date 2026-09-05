//! Tell the agent (and through it, the user) what a config.toml edit did.
//!
//! When a user asks jcode to change a setting, the agent writes
//! `~/.jcode/config.toml` and then has to guess whether the change took
//! effect. That guess is where the confusion comes from. Instead, every file
//! write that lands on the active config file appends an explicit report:
//! which keys changed, and whether each one is live in running sessions or
//! needs a restart.

use crate::instruction::notification::Notification;
use std::path::Path;

/// Resolve a path for comparison, falling back to the path as given.
///
/// A config file that does not exist yet cannot be canonicalized, and that is
/// a normal case here (the very first write creates it), so the unresolved
/// path is the correct answer rather than an error to report.
fn comparable(path: &Path) -> std::path::PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => path.to_path_buf(),
    }
}

/// Whether `path` is the config file the running process actually reads.
///
/// Compares resolved paths so `~/.jcode/config.toml`, a relative path, and a
/// symlinked jcode home all resolve to the same file.
fn is_active_config_file(path: &Path) -> bool {
    let Some(config_path) = crate::config::Config::path() else {
        return false;
    };
    comparable(path) == comparable(&config_path)
}

/// Report appended to a tool result after a write to the active config file.
///
/// Returns `None` for non-config files and for edits that changed no settings
/// (comments or formatting), so ordinary writes stay untouched.
pub fn config_edit_notice(
    path: &Path,
    before: &str,
    after: &str,
    working_dir: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    if !is_active_config_file(path) {
        return Ok(None);
    }
    // Force the next config() call to re-read instead of waiting out the
    // staleness throttle, so "live now" is true the moment it is claimed.
    crate::config::Config::invalidate_cache();

    // A config file that no longer parses is silently ignored by
    // `Config::load`, which falls back to defaults. That is the worst possible
    // outcome to leave unreported: the write "succeeded" while every setting
    // in the file quietly stopped applying. Surface it instead.
    if let Err(error) = crate::config::Config::load_strict() {
        let prose = Notification::ConfigEditInvalid {
            path: &path.display().to_string(),
            error: &error.to_string(),
        }
        .render(working_dir)?;
        return Ok(Some(format!("\n\n{prose}")));
    }

    let changes = crate::config::change_report::diff_toml(before, after);
    if changes.is_empty() {
        return Ok(None);
    }
    let restart = changes
        .iter()
        .filter(|change| change.liveness == crate::config::change_report::Liveness::NeedsRestart)
        .map(|change| change.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let prose = if restart.is_empty() {
        Notification::ConfigEditLive.render(working_dir)?
    } else {
        Notification::ConfigEditRestart { keys: &restart }.render(working_dir)?
    };
    let rows = crate::config::change_report::render_change_rows(&changes);
    Ok(Some(format!("\n\n{rows}{prose}")))
}

/// Append [`config_edit_notice`] to a tool output body when applicable.
pub fn append_config_edit_notice(
    body: &mut String,
    path: &Path,
    before: &str,
    after: &str,
    working_dir: Option<&Path>,
) {
    match config_edit_notice(path, before, after, working_dir) {
        Ok(Some(notice)) => body.push_str(&notice),
        Ok(None) => {}
        // The file mutation already succeeded. Preserve that result and report
        // this distinct notification failure rather than falsely undoing it.
        Err(error) => body.push_str(&format!("\n\n[Config edit notification failed: {error}]")),
    }
}

/// Read the config file, treating "absent or unreadable" as empty.
///
/// An absent config file is the normal pre-state for the write that creates
/// it, and an unreadable one is reported by the change summary itself, so
/// there is no error here worth propagating: empty is the meaningful value.
fn read_config_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Watches the active config file across a whole tool invocation.
///
/// Tools that touch several files, or write through several code paths (patch
/// application, moves, deletes), cannot easily thread before/after content to
/// the place that builds the result string. This captures the config file
/// content up front and re-reads it at the end, so a config edit is reported
/// no matter which path produced it.
pub struct ConfigEditWatch {
    path: Option<std::path::PathBuf>,
    before: String,
    working_dir: Option<std::path::PathBuf>,
}

impl ConfigEditWatch {
    /// Snapshot the active config file before a tool runs.
    pub fn begin(working_dir: Option<std::path::PathBuf>) -> Self {
        let path = crate::config::Config::path();
        let before = match path.as_deref() {
            Some(path) => read_config_text(path),
            None => String::new(),
        };
        Self {
            path,
            before,
            working_dir,
        }
    }

    /// Append a change report if the config file changed while the tool ran.
    pub fn finish(self, body: &mut String) {
        let Some(path) = self.path else {
            return;
        };
        let after = read_config_text(&path);
        if after == self.before {
            return;
        }
        append_config_edit_notice(
            body,
            &path,
            &self.before,
            &after,
            self.working_dir.as_deref(),
        );
    }
}

#[cfg(test)]
#[path = "config_edit_notice_tests.rs"]
mod tests;
