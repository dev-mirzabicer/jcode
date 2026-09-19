use super::{ProjectFacts, ProjectKey, ProjectResolutionError};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn resolve_project(launch_dir: &Path) -> Result<ProjectFacts, ProjectResolutionError> {
    let active_dir = std::fs::canonicalize(launch_dir).map_err(|error| ProjectResolutionError {
        path: launch_dir.to_path_buf(),
        detail: format!("could not canonicalize launch directory: {error}"),
    })?;
    if !active_dir.is_dir() {
        return Err(ProjectResolutionError {
            path: active_dir,
            detail: "launch path is not a directory".to_string(),
        });
    }
    validate_absolute_utf8_path(&active_dir, "canonical launch directory").map_err(|error| {
        ProjectResolutionError {
            path: active_dir.clone(),
            detail: error.to_string(),
        }
    })?;

    match discover_git_project(&active_dir)? {
        Some((active_root, common_dir)) => Ok(ProjectFacts::new(
            ProjectKey::Git {
                canonical_common_dir: common_dir,
            },
            active_root,
        )),
        None => Ok(ProjectFacts::new(
            ProjectKey::Directory {
                canonical_root: active_dir.clone(),
            },
            active_dir,
        )),
    }
}

fn discover_git_project(
    launch_dir: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, ProjectResolutionError> {
    let marker = nearest_git_marker(launch_dir)?;
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
            if marker.is_some() {
                return Err(git_command_error(
                    launch_dir,
                    "rev-parse --show-toplevel",
                    &output,
                ));
            }
            return Ok(None);
        }
        Err(error) => {
            if marker.is_some() {
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
    if let Some(marker) = marker
        && marker.parent() != Some(active_root.as_path())
    {
        return Err(git_identity_error(
            launch_dir,
            "Git discovery skipped a closer .git marker; repair that location before use",
        ));
    }
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
    let mut command = Command::new("git");
    command.args(args).current_dir(launch_dir);
    // Physical discovery must describe this directory. Keep ordinary Git trust
    // configuration, but not process-local repository/index/object redirection.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(key);
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
}

fn parse_git_path(
    launch_dir: &Path,
    stdout: &[u8],
    label: &str,
) -> Result<PathBuf, ProjectResolutionError> {
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
) -> Result<PathBuf, ProjectResolutionError> {
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

fn nearest_git_marker(path: &Path) -> Result<Option<PathBuf>, ProjectResolutionError> {
    for ancestor in path.ancestors() {
        let marker = ancestor.join(".git");
        match std::fs::symlink_metadata(&marker) {
            Ok(_) => return Ok(Some(marker)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(git_identity_error(
                    path,
                    format!("cannot inspect {}: {error}", marker.display()),
                ));
            }
        }
    }
    Ok(None)
}

fn git_command_error(launch_dir: &Path, command: &str, output: &Output) -> ProjectResolutionError {
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

fn git_identity_error(path: &Path, detail: impl Into<String>) -> ProjectResolutionError {
    ProjectResolutionError {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn validate_absolute_utf8_path(path: &Path, label: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{label} is not absolute: {}", path.display()));
    }
    if path.to_str().is_none() {
        return Err(format!("{label} is not valid UTF-8: {}", path.display()));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn dangling_git_marker_is_damage_not_a_non_git_project() {
        let temp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(temp.path().join("missing"), temp.path().join(".git")).unwrap();
        assert!(resolve_project(temp.path()).is_err());
    }

    #[test]
    fn nested_damaged_marker_does_not_adopt_enclosing_repository() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(temp.path())
                .status()
                .unwrap()
                .success()
        );
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::os::unix::fs::symlink(nested.join("missing"), nested.join(".git")).unwrap();
        assert!(resolve_project(&nested).is_err());
    }

    #[test]
    fn ambient_git_redirection_cannot_change_physical_identity() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = temp.path().join("foreign");
        let requested = temp.path().join("requested");
        std::fs::create_dir(&requested).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(&foreign)
                .status()
                .unwrap()
                .success()
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "location::resolve::tests::ambient_git_redirection_child",
            ])
            .env("JCODE_LOCATION_REQUESTED", &requested)
            .env("GIT_DIR", foreign.join(".git"))
            .env("GIT_WORK_TREE", &foreign)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn ambient_git_redirection_child() {
        let Some(path) = std::env::var_os("JCODE_LOCATION_REQUESTED") else {
            return;
        };
        let path = PathBuf::from(path).canonicalize().unwrap();
        let project = resolve_project(&path).unwrap();
        assert!(!project.key().is_git());
        assert_eq!(project.active_root(), path);
    }
}
