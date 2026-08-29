use super::types::{ActiveProject, ProjectKey, StartupContextError, validate_absolute_utf8_path};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub(super) fn resolve_project(launch_dir: &Path) -> Result<ActiveProject, StartupContextError> {
    let active_dir = std::fs::canonicalize(launch_dir).map_err(|error| {
        StartupContextError::ProjectIdentity {
            path: launch_dir.to_path_buf(),
            detail: format!("could not canonicalize launch directory: {error}"),
        }
    })?;
    if !active_dir.is_dir() {
        return Err(StartupContextError::ProjectIdentity {
            path: active_dir,
            detail: "launch path is not a directory".to_string(),
        });
    }
    validate_absolute_utf8_path(&active_dir, "canonical launch directory").map_err(|error| {
        StartupContextError::ProjectIdentity {
            path: active_dir.clone(),
            detail: error.to_string(),
        }
    })?;

    match discover_git_project(&active_dir)? {
        Some((active_root, common_dir)) => Ok(ActiveProject::new(
            ProjectKey::Git {
                canonical_common_dir: common_dir,
            },
            active_root,
        )),
        None => Ok(ActiveProject::new(
            ProjectKey::Directory {
                canonical_root: active_dir.clone(),
            },
            active_dir,
        )),
    }
}

fn discover_git_project(
    launch_dir: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, StartupContextError> {
    let top_level = run_git(launch_dir, ["rev-parse", "--show-toplevel"]);
    let top_level = match top_level {
        Ok(output) if output.status.success() => {
            parse_git_path(launch_dir, &output.stdout, "top-level")?
        }
        Ok(output) => {
            if is_bare_repository(launch_dir) {
                return Err(git_identity_error(
                    launch_dir,
                    "bare Git repositories have no active worktree root",
                ));
            }
            if nearest_git_marker(launch_dir).is_some() {
                return Err(git_command_error(
                    launch_dir,
                    "rev-parse --show-toplevel",
                    &output,
                ));
            }
            return Ok(None);
        }
        Err(error) => {
            if nearest_git_marker(launch_dir).is_some() {
                return Err(git_identity_error(
                    launch_dir,
                    format!("could not invoke Git for a repository: {error}"),
                ));
            }
            return Ok(None);
        }
    };

    let common_output =
        run_git(launch_dir, ["rev-parse", "--git-common-dir"]).map_err(|error| {
            git_identity_error(launch_dir, format!("could not invoke Git: {error}"))
        })?;
    if !common_output.status.success() {
        return Err(git_command_error(
            launch_dir,
            "rev-parse --git-common-dir",
            &common_output,
        ));
    }
    let common_dir = parse_git_path(launch_dir, &common_output.stdout, "common directory")?;

    let active_root = canonical_git_path(launch_dir, top_level, "Git worktree root")?;
    let common_dir = canonical_git_path(launch_dir, common_dir, "Git common directory")?;
    Ok(Some((active_root, common_dir)))
}

fn is_bare_repository(launch_dir: &Path) -> bool {
    run_git(launch_dir, ["rev-parse", "--is-bare-repository"])
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_single_line(&output.stdout).ok())
        .is_some_and(|value| value == "true")
}

fn run_git<I, S>(launch_dir: &Path, args: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .args(args)
        .current_dir(launch_dir)
        .output()
}

fn parse_git_path(
    launch_dir: &Path,
    stdout: &[u8],
    label: &str,
) -> Result<PathBuf, StartupContextError> {
    let value = parse_single_line(stdout).map_err(|detail| {
        git_identity_error(launch_dir, format!("invalid Git {label}: {detail}"))
    })?;
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() {
        path
    } else {
        launch_dir.join(path)
    })
}

fn parse_single_line(stdout: &[u8]) -> Result<String, String> {
    let value = std::str::from_utf8(stdout).map_err(|_| "output path is not valid UTF-8")?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.is_empty() {
        return Err("output path is empty".to_string());
    }
    if value.contains(['\n', '\r']) {
        return Err("output contains more than one line".to_string());
    }
    Ok(value.to_string())
}

fn canonical_git_path(
    launch_dir: &Path,
    path: PathBuf,
    label: &str,
) -> Result<PathBuf, StartupContextError> {
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        git_identity_error(
            launch_dir,
            format!("could not canonicalize {label} {}: {error}", path.display()),
        )
    })?;
    validate_absolute_utf8_path(&canonical, label)
        .map_err(|error| git_identity_error(launch_dir, error.to_string()))?;
    Ok(canonical)
}

fn nearest_git_marker(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .map(|ancestor| ancestor.join(".git"))
        .find(|candidate| candidate.is_dir() || candidate.is_file())
}

fn git_command_error(launch_dir: &Path, command: &str, output: &Output) -> StartupContextError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    git_identity_error(
        launch_dir,
        format!(
            "git {command} failed with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        ),
    )
}

fn git_identity_error(path: &Path, detail: impl Into<String>) -> StartupContextError {
    StartupContextError::ProjectIdentity {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}
