//! Tier 3/4: clipboard, scripting bridge, waits, notifications, system state.

use super::osa;
use crate::execution::helper::HelperHost;
use anyhow::{Result, bail};
use jcode_tool_types::ToolOutput;
use serde_json::json;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tokio::process::Command;

pub fn get_clipboard(host: &HelperHost) -> Result<ToolOutput> {
    // pbpaste is the most reliable text read.
    let out = host.command("/usr/bin/pbpaste", &[], None)?;
    anyhow::ensure!(
        out.status.success(),
        "pbpaste failed; helper output is retained"
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok(ToolOutput::new(text).with_title("clipboard"))
}

pub fn set_clipboard(host: &HelperHost, text: &str) -> Result<ToolOutput> {
    let out = host.program(Command::new("/usr/bin/pbcopy"), Some(text.as_bytes()), None)?;
    anyhow::ensure!(
        out.status.success(),
        "pbcopy failed; helper output is retained"
    );
    Ok(ToolOutput::new(format!(
        "copied {} chars to clipboard",
        text.chars().count()
    )))
}

pub fn run_applescript(host: &HelperHost, script: &str) -> Result<ToolOutput> {
    let out = osa::run_applescript(host, script)?;
    Ok(ToolOutput::new(if out.is_empty() {
        "(AppleScript ran, no output)".to_string()
    } else {
        out
    })
    .with_title("applescript"))
}

pub fn run_jxa(host: &HelperHost, script: &str) -> Result<ToolOutput> {
    let out = osa::run_jxa(host, script)?;
    Ok(ToolOutput::new(if out.is_empty() {
        "(JXA ran, no output)".to_string()
    } else {
        out
    })
    .with_title("jxa"))
}

pub fn notify(host: &HelperHost, text: &str, title: Option<&str>) -> Result<ToolOutput> {
    let title = title.unwrap_or("jcode");
    osa::run_applescript(
        host,
        &format!(
            "display notification {} with title {}",
            osa::as_quote(text),
            osa::as_quote(title)
        ),
    )?;
    Ok(ToolOutput::new(format!("posted notification: {text}")))
}

/// Poll an app's AX tree until a substring appears (element_appears) or a
/// timeout elapses. Cheap structural wait instead of fixed sleeps.
pub fn wait_for(
    host: &HelperHost,
    app: &str,
    contains: &str,
    timeout_ms: u64,
) -> Result<ToolOutput> {
    let total = Duration::from_millis(timeout_ms.min(60_000));
    let deadline = Instant::now() + total;
    // Keep each poll bounded by the per-poll timeout below so it returns even on
    // big apps. Depth 8 captures most visible labels/values while staying cheap.
    let script = format!(
        r#"
using terms from application "System Events"
    on dumpEl(el, lvl, maxlvl)
        set out to ""
        if lvl > maxlvl then return out
        try
            set out to out & (title of el as text) & " "
        end try
        try
            set out to out & (value of el as text) & " "
        end try
        try
            set out to out & (description of el as text) & " "
        end try
        try
            repeat with child in (UI elements of el)
                set out to out & (my dumpEl(child, lvl + 1, maxlvl))
            end repeat
        end try
        return out
    end dumpEl
end using terms from
tell application "System Events"
    set frontApp to first application process whose name is {app}
    try
        return my dumpEl(front window of frontApp, 0, 8)
    on error
        return ""
    end try
end tell
"#,
        app = osa::as_quote(app)
    );
    loop {
        host.check_stop()?;
        // Bound each poll by the time left so the whole call honors timeout_ms,
        // and never let a single poll exceed ~3s.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("wait_for timed out after {timeout_ms}ms (no '{contains}' in {app})");
        }
        let poll_budget = remaining.min(Duration::from_secs(3));
        let tree = osa::run_applescript_timeout(host, &script, poll_budget).unwrap_or_default();
        if tree.contains(contains) {
            return Ok(ToolOutput::new(format!("matched '{contains}' in {app}")));
        }
        if Instant::now() >= deadline {
            bail!("wait_for timed out after {timeout_ms}ms (no '{contains}' in {app})");
        }
        sleep(Duration::from_millis(200));
    }
}

/// Read common system state (battery, display brightness, focus, etc.).
pub fn system_state(host: &HelperHost) -> Result<ToolOutput> {
    let battery_result = host.command("/usr/bin/pmset", &["-g", "batt"], None)?;
    let date_result = host.command("/bin/date", &[], None)?;
    let battery = String::from_utf8_lossy(&battery_result.stdout)
        .trim()
        .to_string();
    let date = String::from_utf8_lossy(&date_result.stdout)
        .trim()
        .to_string();
    Ok(ToolOutput::new(format!("date: {date}\n{battery}"))
        .with_title("system_state")
        .with_error(!battery_result.status.success() || !date_result.status.success())
        .with_metadata(json!({"battery_raw": battery})))
}

/// Set display brightness 0.0..1.0 using the `brightness` cli if present, else
/// fall back to AppleScript key events. Brightness has no stable public API, so
/// this is best-effort.
pub fn set_brightness(host: &HelperHost, level: f64) -> Result<ToolOutput> {
    let level = level.clamp(0.0, 1.0);
    // Try the `brightness` homebrew tool first.
    for path in ["/opt/homebrew/bin/brightness", "/usr/local/bin/brightness"] {
        if std::path::Path::new(path).exists() {
            let ok = host
                .command(path, &[&format!("{level}")], None)
                .map(|output| output.status.success())
                .unwrap_or(false);
            if ok {
                return Ok(ToolOutput::new(format!("set brightness to {level:.2}")));
            }
        }
    }
    bail!(
        "no brightness control available. Install with `brew install brightness`, or adjust via the \
         brightness keys."
    )
}
