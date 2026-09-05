use super::*;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};

struct EnvGuard(&'static str, Option<std::ffi::OsString>);
impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let old = std::env::var_os(key);
        crate::env::set_var(key, value);
        if key == "JCODE_HOME" {
            crate::config::Config::invalidate_cache();
        }
        Self(key, old)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.1.take() {
            Some(value) => crate::env::set_var(self.0, value),
            None => crate::env::remove_var(self.0),
        }
        if self.0 == "JCODE_HOME" {
            crate::config::Config::invalidate_cache();
        }
    }
}

#[test]
fn managed_refusal_never_changes_command_enforcement() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("JCODE_HOME", home.path());
    crate::instruction::SystemPromptComposer::new()
        .ensure_global_store()
        .unwrap();
    let marker = home.path().join("sentinel");
    std::fs::write(&marker, "kept").unwrap();
    let _target = EnvGuard::set("WP06_NOTICE_TARGET", &marker);
    let source = home
        .path()
        .join("instructions/notifications/destructive-command-gate-reflect.md");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for body in [
        Some("SYNTHETIC {{explanation}}"),
        Some(""),
        Some("{{missing}}"),
        None,
    ] {
        if let Some(body) = body {
            std::fs::write(&source, format!("---\nid: destructive-command-gate-reflect\nkind: notification\ntemplate: handlebars\n---\n{body}")).unwrap();
        } else {
            std::fs::remove_file(&source).unwrap();
        }
        let ctx = ToolContext {
            session_id: "test".into(),
            message_id: "test".into(),
            tool_call_id: "test".into(),
            working_dir: Some(home.path().to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        };
        // Even if the gate regresses, this command can only overwrite a
        // disposable fixture, never a user path.
        let result = runtime.block_on(crate::tool::bash::BashTool::new().execute(
            serde_json::json!({"command": "printf unexpected > \"$WP06_NOTICE_TARGET\""}),
            ctx,
        ));
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "kept");
    }
    // An authorized Confirm outcome does not depend on refusal-file health.
    assert!(
        destructive_command_refusal(
            "printf x > \"$WP06_NOTICE_TARGET\"",
            Some("The user requested replacement of this disposable fixture."),
            Some(home.path().to_path_buf())
        )
        .is_none()
    );
    assert!(destructive_command_refusal("ls", None, Some(home.path().to_path_buf())).is_none());
    // Pure assessment only: no shell is invoked for this catastrophic fixture.
    assert!(
        destructive_command_refusal(
            "rm -rf /",
            Some("The user provided a substantive explanation for this test."),
            Some(home.path().to_path_buf())
        )
        .is_some()
    );
}
