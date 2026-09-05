//! Prose for notifications about server-owned state. Failure remains visible
//! without undoing cleanup, elections or already-persisted plan mutations.
use crate::instruction::notification::Notification;
use std::path::Path;

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
        .unwrap_or_else(|error| format!("Coordination notification rendering failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordination_prose_is_current_empty_and_fail_visible() {
        let source = TestHome::new();
        source.write("file-activity-previous", "OLD {{actor}} {{operation}}");
        let old = render(
            Notification::FileActivityPrevious {
                actor: "peer<&>",
                operation: "edited",
            },
            None,
        );
        source.write("file-activity-previous", "NEW {{actor}} {{operation}}");
        assert_eq!(
            render(
                Notification::FileActivityPrevious {
                    actor: "peer<&>",
                    operation: "edited"
                },
                None
            ),
            "NEW peer<&> edited"
        );
        assert_eq!(old, "OLD peer<&> edited");
        source.write("file-activity-previous", "");
        assert_eq!(
            render(
                Notification::FileActivityPrevious {
                    actor: "peer",
                    operation: "edited"
                },
                None
            ),
            ""
        );
        source.write("file-activity-previous", "{{invalid}}");
        assert!(
            render(
                Notification::FileActivityPrevious {
                    actor: "peer",
                    operation: "edited"
                },
                None
            )
            .contains("Coordination notification rendering failed:")
        );
    }
}
