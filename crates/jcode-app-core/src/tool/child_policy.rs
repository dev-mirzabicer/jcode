//! Deliberately narrow native-tool enforcement, not an adversarial OS sandbox.
use super::*;
use anyhow::{Context, ensure};
use jcode_tool_types::delegation::Permission;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct ChildToolPolicy {
    session_id: String,
    permission: Permission,
    artifacts: PathBuf,
    state_root: PathBuf,
}

pub(crate) const ADMIN_TOOLS: &[&str] = &[
    "subagent",
    "get_catalog",
    "selfdev",
    "schedule",
    "debug_socket",
];

/// Installed only from validated Session state, never from tool arguments or prose.
pub(crate) fn bind(session: &crate::session::Session) -> Result<Option<ChildToolPolicy>> {
    session.validate_isolated_child()?;
    let Some(child) = &session.isolated_child else {
        return Ok(None);
    };
    let artifacts = child
        .identity
        .artifact_dir
        .canonicalize()
        .context("Resolve child artifact directory")?;
    ensure!(artifacts.is_dir(), "Child artifacts are not a directory");
    ensure!(
        artifacts == child.identity.artifact_dir
            && !std::fs::symlink_metadata(&child.identity.artifact_dir)?
                .file_type()
                .is_symlink(),
        "Child artifact directory identity changed; permission was not rebound to a new target"
    );
    let state_root = crate::storage::jcode_dir()?.canonicalize()?;
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|p| p.into_inner());
    let policy = policies
        .get_mut(&session.id)
        .context("Child has no bound tool registry policy")?;
    let bound = ChildToolPolicy {
        session_id: session.id.clone(),
        permission: child.permission,
        artifacts,
        state_root,
    };
    policy.child = Some(bound.clone());
    policy
        .disabled_tools
        .extend(ADMIN_TOOLS.iter().map(|name| name.to_string()));
    Ok(Some(bound))
}

pub(crate) fn authorize(
    bound: Option<&ChildToolPolicy>,
    ctx: &ToolContext,
    name: &str,
    input: &Value,
) -> Result<()> {
    let fallback = session_tool_policy(&ctx.session_id).and_then(|policy| policy.child);
    let Some(child) = bound.or(fallback.as_ref()) else {
        return Ok(());
    };
    ensure!(
        child.session_id == ctx.session_id,
        "Child Registry cannot be rebound to another caller"
    );
    ensure!(
        !ADMIN_TOOLS.contains(&name),
        "Harness administration and recursive delegation belong to the parent"
    );
    ensure!(
        !(name == "bg" && input.get("action").and_then(Value::as_str) == Some("cleanup")),
        "Shared execution cleanup belongs to the parent"
    );
    if name == "initiative" {
        ensure!(
            !matches!(
                input.get("action").and_then(Value::as_str),
                Some("create" | "update" | "checkpoint")
            ),
            "Project and global initiative updates belong to the parent after it reviews the child result"
        );
    }
    if name == "skill_manage" {
        ensure!(
            !matches!(
                input.get("action").and_then(Value::as_str),
                Some("reload_all" | "reload")
            ),
            "Harness-wide skill reload belongs to the parent"
        );
    }
    let paths = match name {
        "write" | "edit" | "multiedit" => vec![
            input
                .get("file_path")
                .and_then(Value::as_str)
                .context("Mutation requires file_path")?
                .to_string(),
        ],
        "patch" | "apply_patch" => parsed_patch_file_paths(
            name,
            input
                .get("patch_text")
                .and_then(Value::as_str)
                .context("Mutation requires patch_text")?,
        )?,
        _ => Vec::new(),
    };
    // All destinations (including both sides of a move) precede every effect.
    for path in paths {
        child.check_path(&ctx.resolve_path(Path::new(&path)))?;
    }
    Ok(())
}

pub(crate) fn authorize_task_control(
    ctx: &ToolContext,
    run: &str,
    target_session: &str,
) -> Result<()> {
    if ctx.session_id == target_session {
        return Ok(());
    }
    ensure!(
        !ctx.invocation.isolated_child,
        "Children can control only their own tool work"
    );
    let root = crate::storage::jcode_dir()?;
    if let Some(policy) = session_tool_policy(&ctx.session_id)
        && policy.child.is_some()
    {
        anyhow::bail!(
            "Children can control only their own tool work, not parent or unrelated executions"
        );
    }
    let store = crate::execution::ExecutionStore::open(&root)?;
    ensure!(
        store.child_for_run(run)?.is_none(),
        "Only the original parent can control this child run; inherited history does not transfer control"
    );
    if root
        .join("sessions")
        .join(format!("{target_session}.json"))
        .is_file()
    {
        let (_, original_parent) =
            crate::session::Session::inspection_relationships(&root, target_session, None)?;
        if let Some(original_parent) = original_parent {
            ensure!(
                original_parent == ctx.session_id,
                "Child tool work belongs to a different original parent"
            );
        }
    }
    Ok(())
}

impl ChildToolPolicy {
    fn check_path(&self, path: &Path) -> Result<()> {
        ensure!(
            path.is_absolute(),
            "Child mutation requires a bound working directory"
        );
        ensure!(
            !path
                .components()
                .any(|part| matches!(part, Component::ParentDir)),
            "Resolve child mutation traversal to an explicit path first"
        );
        let mut ancestor = path;
        let mut missing = Vec::new();
        loop {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) => {
                    ensure!(
                        !metadata.file_type().is_symlink(),
                        "Child mutation may not target a symlink"
                    );
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(
                        ancestor
                            .file_name()
                            .context("Mutation target has no filename")?
                            .to_os_string(),
                    );
                    ancestor = ancestor
                        .parent()
                        .context("Mutation target has no existing ancestor")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut resolved = ancestor.canonicalize()?;
        for component in missing.iter().rev() {
            resolved.push(component);
        }
        let artifact = resolved.starts_with(&self.artifacts) && resolved != self.artifacts;
        ensure!(
            self.permission == Permission::ReadWrite || artifact,
            "Read-only child mutations are restricted to {}",
            self.artifacts.display()
        );
        ensure!(
            !resolved.starts_with(&self.state_root)
                || resolved.starts_with(self.state_root.join("scratch"))
                || artifact,
            "Child cannot modify harness state, configuration or model policy. Use its artifact directory or ask the parent."
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_policy_rejects_external_traversal_and_symlink_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let artifacts = state.join("artifacts/child");
        std::fs::create_dir_all(&artifacts).unwrap();
        let policy = ChildToolPolicy {
            session_id: "fixture".into(),
            permission: Permission::ReadOnly,
            artifacts: artifacts.canonicalize().unwrap(),
            state_root: state.canonicalize().unwrap(),
        };
        assert!(policy.check_path(&artifacts.join("nested/new.md")).is_ok());
        assert!(policy.check_path(&temp.path().join("project.md")).is_err());
        assert!(policy.check_path(&artifacts.join("../outside.md")).is_err());
        assert!(policy.check_path(&artifacts).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), artifacts.join("escape")).unwrap();
            assert!(policy.check_path(&artifacts.join("escape/new.md")).is_err());
        }
        let writable = ChildToolPolicy {
            permission: Permission::ReadWrite,
            ..policy
        };
        assert!(writable.check_path(&temp.path().join("project.md")).is_ok());
        assert!(writable.check_path(&state.join("config.toml")).is_err());
    }
}
