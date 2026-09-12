//! Centralized `osascript` / JXA execution for the `macos_computer_use` tool.
//!
//! Many macOS capabilities (Accessibility actions, window/app management, system
//! state) are reachable through AppleScript / JavaScript-for-Automation without
//! extra native bindings. This module funnels all of that through one place so
//! escaping, error mapping (especially the TCC permission errors), and timeouts
//! are handled consistently.
//!
//! Every external command runs under a wall-clock timeout: a hung target app
//! must never freeze the agent. AppleScript also gets an internal
//! `with timeout` guard so System Events stops waiting on an unresponsive app.

use crate::execution::helper::HelperHost;
use anyhow::{Result, bail};
use std::time::Duration;

/// Default wall-clock limit for a scripting call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Run an AppleScript and return stdout (trimmed). Maps the common macOS
/// permission / automation errors to actionable messages.
pub fn run_applescript(host: &HelperHost, script: &str) -> Result<String> {
    run(host, &["-e", script], "AppleScript", DEFAULT_TIMEOUT)
}

/// Run AppleScript with an explicit timeout.
pub fn run_applescript_timeout(
    host: &HelperHost,
    script: &str,
    timeout: Duration,
) -> Result<String> {
    run(host, &["-e", script], "AppleScript", timeout)
}

/// Run a JavaScript-for-Automation (JXA) script.
pub fn run_jxa(host: &HelperHost, script: &str) -> Result<String> {
    run(
        host,
        &["-l", "JavaScript", "-e", script],
        "JXA",
        DEFAULT_TIMEOUT,
    )
}

fn run(host: &HelperHost, args: &[&str], lang: &str, timeout: Duration) -> Result<String> {
    let (status, stdout, stderr) = run_command_timed(host, "/usr/bin/osascript", args, timeout)?;

    if status {
        return Ok(stdout.trim_end().to_string());
    }

    bail!("{}", classify_osa_error(stderr.trim(), lang));
}

/// Map an osascript stderr message to an actionable error string. Pure so it can
/// be unit-tested without shelling out. Ordering matters: permission errors are
/// the most actionable and have distinctive text, so they are checked before the
/// generic "reference does not resolve" index errors.
fn classify_osa_error(trimmed: &str, lang: &str) -> String {
    let lower = trimmed.to_lowercase();

    // Permission errors first. The real "not allowed assistive access" denial
    // always carries that phrase; -25211 is errAXAPIDisabled.
    if lower.contains("assistive")
        || lower.contains("not allowed")
        || lower.contains("-25211")
        || lower.contains("1002")
    {
        return format!(
            "Accessibility permission required. Run the `setup` action, or grant it in \
             System Settings > Privacy & Security > Accessibility for your terminal/jcode. \
             ({trimmed})"
        );
    }
    if lower.contains("-1743") || lower.contains("not authorized to send apple events") {
        return format!(
            "Automation permission required for the target app. Approve the prompt, or grant it \
             in System Settings > Privacy & Security > Automation. ({trimmed})"
        );
    }
    // -1719 (errAEIllegalIndex / "Invalid index") and -1728 (errAENoSuchObject /
    // "Can't get ...") mean the target reference does not resolve: the app isn't
    // running, has no front window, or an AX path index is out of range. This is
    // NOT a permission problem, so report it accurately instead of sending the
    // user to grant Accessibility (the previous behavior, which was misleading).
    if lower.contains("-1719") || lower.contains("-1728") || lower.contains("invalid index") {
        return format!(
            "target not found: the app may not be running, has no front window, or an AX path \
             index is out of range. Check `list_apps`/`ui` and retry. ({trimmed})"
        );
    }
    if trimmed.is_empty() {
        return format!("{lang} failed (no error output)");
    }
    format!("{lang} failed: {trimmed}")
}

/// Run a command with a wall-clock timeout. Returns (success, stdout, stderr).
/// On timeout the child is killed and an error is returned.
pub fn run_command_timed(
    host: &HelperHost,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String, String)> {
    let output = host.command(program, args, Some(timeout))?;
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        anyhow::anyhow!(
            "Helper stdout is not UTF-8; exact bytes remain in the retained helper parts"
        )
    })?;
    let stderr = String::from_utf8(output.stderr).map_err(|_| {
        anyhow::anyhow!(
            "Helper stderr is not UTF-8; exact bytes remain in the retained helper parts"
        )
    })?;
    Ok((output.status.success(), stdout, stderr))
}

/// Quote a string as an AppleScript string literal (wraps in quotes, escapes
/// backslash and double-quote). Use for interpolating untrusted text into
/// generated AppleScript.
pub fn as_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, tokio::runtime::Runtime, HelperHost) {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let entered = runtime.enter();
        let mut context = super::super::ToolContext {
            session_id: "osa-test".into(),
            message_id: crate::id::new_id("osa"),
            tool_call_id: "command".into(),
            working_dir: Some(directory.path().into()),
            stdin_request_tx: None,
            graceful_shutdown_signal: Some(jcode_agent_runtime::InterruptSignal::new()),
            execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
            invocation: Default::default(),
        };
        let store = crate::execution::ExecutionStore::open(directory.path()).unwrap();
        let invocation = crate::execution::invocation(&context, "osa-test", serde_json::json!({}));
        let crate::execution::PreparedInvocation::New(record) =
            store.prepare(&invocation, "test-owner").unwrap()
        else {
            panic!()
        };
        store.start(&record.id, "test-owner").unwrap();
        context.invocation.capture = Some(std::sync::Arc::new(
            crate::execution::Capture::create(store, record, Default::default()).unwrap(),
        ));
        let host = HelperHost::new(&context, "osa").unwrap();
        drop(entered);
        (directory, runtime, host)
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(as_quote("hi"), "\"hi\"");
        assert_eq!(as_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(as_quote("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn timed_command_succeeds_fast() {
        let (_directory, _runtime, host) = fixture();
        let (ok, out, _) =
            run_command_timed(&host, "/bin/echo", &["hi"], Duration::from_secs(5)).unwrap();
        assert!(ok);
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn timed_command_drains_both_pipes_before_waiting_for_exit() {
        let (_directory, _runtime, host) = fixture();
        let (success,stdout,stderr)=run_command_timed(&host,
            "/usr/bin/python3",
            &["-c","import os; os.write(1, b'x' * 200000 + b'OUT_TAIL'); os.write(2, b'y' * 200000 + b'ERR_TAIL')"],
            Duration::from_secs(3),
        ).expect("A full pipe must not turn a completed-output command into a timeout");
        assert!(success);
        assert_eq!(stdout, format!("{}OUT_TAIL", "x".repeat(200000)));
        assert_eq!(stderr, format!("{}ERR_TAIL", "y".repeat(200000)));
    }

    #[test]
    fn timed_command_times_out() {
        let (_directory, _runtime, host) = fixture();
        let err = run_command_timed(&host, "/bin/sleep", &["5"], Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn helper_stdin_is_retained_and_read_without_a_pipe_writer_deadlock() {
        let (directory, _runtime, host) = fixture();
        let bytes = vec![b'x'; 300000];
        let mut command = tokio::process::Command::new("/usr/bin/python3");
        command.args(["-c","import sys; data=sys.stdin.buffer.read(); sys.stdout.buffer.write(data); sys.stderr.write('INPUT_DONE')"]);
        let output = host
            .program(command, Some(&bytes), Some(Duration::from_secs(3)))
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, bytes);
        assert_eq!(output.stderr, b"INPUT_DONE");
        let store = crate::execution::ExecutionStore::open(directory.path()).unwrap();
        let record = store.list("osa-test", None, 10).unwrap().pop().unwrap();
        let path = record
            .output_path
            .unwrap()
            .with_file_name("part-osa-0-stdin.bin");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn classifies_permission_error() {
        let msg = classify_osa_error(
            "execution error: System Events got an error: osascript is not allowed assistive access. (-25211)",
            "AppleScript",
        );
        assert!(
            msg.contains("Accessibility permission required"),
            "got: {msg}"
        );
    }

    #[test]
    fn classifies_invalid_index_as_not_found_not_permission() {
        // Regression: -1719 used to be misreported as "Accessibility permission
        // required" even though it means the target reference didn't resolve.
        let msg = classify_osa_error(
            "execution error: System Events got an error: Can\u{2019}t get application process 1 whose name = \"Codex\". Invalid index. (-1719)",
            "AppleScript",
        );
        assert!(msg.contains("target not found"), "got: {msg}");
        assert!(!msg.contains("Accessibility permission"), "got: {msg}");
    }

    #[test]
    fn classifies_no_such_object_as_not_found() {
        let msg = classify_osa_error(
            "execution error: System Events got an error: Can\u{2019}t get front window of process \"Foo\". (-1728)",
            "AppleScript",
        );
        assert!(msg.contains("target not found"), "got: {msg}");
    }

    #[test]
    fn classifies_automation_error() {
        let msg = classify_osa_error(
            "execution error: Not authorized to send Apple events to Finder. (-1743)",
            "AppleScript",
        );
        assert!(msg.contains("Automation permission required"), "got: {msg}");
    }

    #[test]
    fn classifies_generic_error() {
        let msg = classify_osa_error("execution error: something weird (-2700)", "JXA");
        assert!(msg.contains("JXA failed"), "got: {msg}");
    }
}
