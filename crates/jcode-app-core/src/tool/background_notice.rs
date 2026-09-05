//! Guidance attached to existing background results or blocked validation.
//! A prose failure must not discard an already-created task's receipt or data.

use crate::background::BackgroundTaskInfo;
use crate::instruction::notification::Notification;
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
pub(super) struct TestHome {
    home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}
#[cfg(test)]
impl TestHome {
    pub(super) fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::config::Config::invalidate_cache();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        Self {
            home,
            previous,
            _guard: guard,
        }
    }
    pub(super) fn write(&self, id: &str, body: &str) {
        std::fs::write(
            self.home
                .path()
                .join(format!("instructions/notifications/{id}.md")),
            format!("---\nid: {id}\nkind: notification\ntemplate: handlebars\n---\n{body}"),
        )
        .unwrap();
    }
}
#[cfg(test)]
impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(old) => crate::env::set_var("JCODE_HOME", old),
            None => crate::env::remove_var("JCODE_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

pub(super) fn render(notice: Notification<'_>, working_dir: Option<&Path>) -> String {
    notice
        .render(working_dir)
        .unwrap_or_else(|error| format!("Background instruction rendering failed: {error}"))
}

pub(super) fn promoted(
    info: &BackgroundTaskInfo,
    name: &str,
    timeout_ms: u64,
    elapsed: Option<Duration>,
    working_dir: Option<&Path>,
) -> String {
    let (suffix, foreground, notice) = match elapsed {
        Some(elapsed) => (
            "",
            format!("Foreground time used: {:.1}s\n", elapsed.as_secs_f64()),
            Notification::BashDetachedPromoted {
                task_id: &info.task_id,
            },
        ),
        None => (
            " (not killed)",
            String::new(),
            Notification::BashForegroundPromoted {
                task_id: &info.task_id,
            },
        ),
    };
    let prose = render(notice, working_dir);
    format!(
        "Command exceeded the foreground timeout after {:.1}s and is continuing in background{suffix}.\n\nTask ID: {}\nName: {name}\n{foreground}Output file: {}\nStatus file: {}\n\n{prose}",
        timeout_ms as f64 / 1000.0,
        info.task_id,
        info.output_file.display(),
        info.status_file.display()
    )
}

pub(super) fn reloaded(info: &BackgroundTaskInfo, working_dir: Option<&Path>) -> String {
    let prose = render(
        Notification::BashReloadBackground {
            task_id: &info.task_id,
        },
        working_dir,
    );
    format!(
        "Command continued in background due to reload.\n\nTask ID: {}\nOutput file: {}\nStatus file: {}\n\n{prose}",
        info.task_id,
        info.output_file.display(),
        info.status_file.display()
    )
}

pub(super) fn started(
    info: &BackgroundTaskInfo,
    name: &str,
    notify: bool,
    wake: bool,
    working_dir: Option<&Path>,
) -> String {
    let delivery = render(
        if wake {
            Notification::BashBackgroundWake
        } else if notify {
            Notification::BashBackgroundNotify
        } else {
            Notification::BashBackgroundSilent
        },
        working_dir,
    );
    let advice = render(
        Notification::BashBackgroundStarted {
            task_id: &info.task_id,
        },
        working_dir,
    );
    let progress = render(Notification::BashBackgroundProgress, working_dir);
    format!(
        "Command started in background.\n\nTask ID: {}\nName: {name}\nOutput file: {}\nStatus file: {}\n\n{delivery}\n{advice}\n\n{progress}",
        info.task_id,
        info.output_file.display(),
        info.status_file.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_guidance_is_current_and_failures_keep_receipt_fields() {
        let source = TestHome::new();
        let info = BackgroundTaskInfo {
            task_id: "fixture-task".into(),
            output_file: "fixture.output".into(),
            status_file: "fixture.status".into(),
        };
        source.write("bash-detached-promoted", "OLD {{task_id}}");
        let old = promoted(
            &info,
            "fixture",
            100,
            Some(Duration::from_millis(125)),
            None,
        );
        source.write("bash-detached-promoted", "NEW {{task_id}}");
        let current = promoted(
            &info,
            "fixture",
            100,
            Some(Duration::from_millis(125)),
            None,
        );
        assert!(old.ends_with("OLD fixture-task"));
        assert!(current.ends_with("NEW fixture-task"));
        source.write("bash-detached-promoted", "{{invalid}}");
        let failed = promoted(
            &info,
            "fixture",
            100,
            Some(Duration::from_millis(125)),
            None,
        );
        assert!(failed.contains("Task ID: fixture-task"));
        assert!(failed.contains("Output file: fixture.output"));
        assert!(failed.contains("Status file: fixture.status"));
        assert!(failed.contains("Background instruction rendering failed:"));
        source.write("bash-detached-promoted", "");
        assert!(
            promoted(
                &info,
                "fixture",
                100,
                Some(Duration::from_millis(125)),
                None
            )
            .ends_with("Status file: fixture.status\n\n")
        );
    }
}
