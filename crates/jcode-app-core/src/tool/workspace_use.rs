//! Lifetime admission, not a new cross-checkout read permission. Mutation
//! destinations still use the concrete native permit/parser owner.
use super::ToolContext;
use crate::workspace::WorkspaceService;
use anyhow::Result;
use jcode_base::workspace::WorkspaceUseLease;
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn acquire(
    workspace: &WorkspaceService,
    name: &str,
    input: &Value,
    ctx: &ToolContext,
) -> Result<WorkspaceUseLease> {
    let keys: &[&str] = match name {
        "read" | "write" | "edit" | "multiedit" | "side_panel" => &["file_path"],
        "ls" => &["path"],
        "agentgrep" => &["path", "file"],
        "open" => &["target"],
        _ => &[],
    };
    let cwd = match &ctx.working_dir {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir()?,
    };
    let mut targets = Vec::new();
    for key in keys {
        if let Some(value) = input.get(*key).and_then(Value::as_str) {
            if name == "open" && value.contains("://") {
                continue;
            }
            let path = PathBuf::from(value);
            targets.push(if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            });
        }
    }
    // Omitted exploration roots mean the invocation cwd, including descendants.
    if targets.is_empty() && matches!(name, "ls" | "agentgrep") {
        targets.push(cwd.clone());
    }
    Ok(workspace.acquire_location_use(Some(&cwd), &targets)?)
}
