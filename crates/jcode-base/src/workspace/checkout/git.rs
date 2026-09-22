use super::*;
use crate::execution::owned_child::OwnedChild;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::ffi::OsStr;
use std::process::{Command, Stdio};
use tokio::io::AsyncReadExt;

fn reference(base: &CloneBase) -> Result<String> {
    match base {
        CloneBase::Branch { name } | CloneBase::Tag { name } => {
            if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_control) {
                return Err(issue(IssueCode::InvalidInput, "Invalid Git ref name"));
            }
            let prefix = if matches!(base, CloneBase::Branch { .. }) {
                "refs/heads"
            } else {
                "refs/tags"
            };
            let full = format!("{prefix}/{name}");
            let output = git(None, [OsStr::new("check-ref-format"), OsStr::new(&full)])
                .output()
                .map_err(io)?;
            if !output.status.success() {
                return Err(issue(IssueCode::InvalidInput, "Invalid Git ref name"));
            }
            Ok(full)
        }
        CloneBase::Commit { oid } => {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Commit must be a complete hexadecimal object ID",
                ));
            }
            Ok(oid.to_ascii_lowercase())
        }
    }
}

pub(super) fn validate_branch(name: &str) -> Result<()> {
    let _ = reference(&CloneBase::Branch {
        name: name.to_owned(),
    })?;
    Ok(())
}

pub(super) fn resolve_base(source: &CloneSource, base: &CloneBase) -> Result<String> {
    let reference = reference(base)?;
    let value = match source {
        CloneSource::Local { path } => {
            let peel = format!("{reference}^{{commit}}");
            let output = git(
                Some(path),
                [
                    OsStr::new("rev-parse"),
                    OsStr::new("--verify"),
                    OsStr::new(&peel),
                ],
            )
            .output()
            .map_err(io)?;
            success_oid(output)?
        }
        CloneSource::Remote { url } => {
            if matches!(base, CloneBase::Commit { .. }) {
                // An arbitrary unreachable object is not proof of a reviewed base.
                // Inspect advertised refs; acquisition later verifies the commit object.
                let output = git(
                    None,
                    [
                        OsStr::new("ls-remote"),
                        OsStr::new(url),
                        OsStr::new(&reference),
                    ],
                )
                .output()
                .map_err(io)?;
                let lines = parse_ls_remote(output)?;
                if !lines.iter().any(|(oid, _)| oid == &reference) {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Commit is not advertised by this source; select a branch or tag, or an existing local checkout",
                    ));
                }
                reference
            } else {
                let peeled = format!("{reference}^{{}}");
                let output = git(
                    None,
                    [
                        OsStr::new("ls-remote"),
                        OsStr::new(url),
                        OsStr::new(&reference),
                        OsStr::new(&peeled),
                    ],
                )
                .output()
                .map_err(io)?;
                let lines = parse_ls_remote(output)?;
                let chosen = lines
                    .iter()
                    .find(|(_, name)| name == &peeled)
                    .or_else(|| lines.iter().find(|(_, name)| name == &reference))
                    .ok_or_else(|| {
                        issue(
                            IssueCode::InvalidInput,
                            "Selected branch/tag is not advertised by the source",
                        )
                    })?;
                chosen.0.clone()
            }
        }
    };
    Ok(value)
}

fn success_oid(output: std::process::Output) -> Result<String> {
    if !output.status.success() {
        return Err(issue(
            IssueCode::InvalidInput,
            "Git source does not contain the selected commit",
        ));
    }
    let value = std::str::from_utf8(&output.stdout).map_err(io)?.trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git returned an invalid object ID",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn parse_ls_remote(output: std::process::Output) -> Result<Vec<(String, String)>> {
    if !output.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git could not inspect the selected source ref; verify authentication and source availability",
        ));
    }
    if output.stdout.len() > 1024 * 1024 {
        return Err(issue(
            IssueCode::InvalidInput,
            "Git ref listing is unexpectedly large",
        ));
    }
    std::str::from_utf8(&output.stdout)
        .map_err(io)?
        .lines()
        .map(|line| {
            let (oid, name) = line
                .split_once('\t')
                .ok_or_else(|| corrupt("Malformed Git ref listing"))?;
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(corrupt("Git ref listing has an invalid object ID"));
            }
            Ok((oid.to_ascii_lowercase(), name.to_owned()))
        })
        .collect()
}

/// Restrict Git environment without evaluating repository hooks, ambient Git
/// redirection, global filters, credential command helpers or SSH overrides.
/// macOS keychain and the user's ordinary SSH agent retain credential custody.
pub(super) fn git<I, S>(cwd: Option<&Path>, arguments: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env(
            "GIT_SSH_COMMAND",
            "/usr/bin/ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
        )
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "filter.lfs.smudge=",
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.required=false",
        ]);
    #[cfg(target_os = "macos")]
    command.args(["-c", "credential.helper=osxkeychain"]);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
}

/// One Git process has an owned process group and a durable execution ticket.
/// Cancellation polls catalog intent rather than coupling work to a client wait.
pub(super) async fn run(
    service: &WorkspaceService,
    request: RequestId,
    command: Command,
    capture: &dyn OutputCapture,
) -> Result<()> {
    let mut command = tokio::process::Command::from(command);
    let ticket = capture.begin_process().map_err(io)?;
    let mut child = match OwnedChild::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => {
            capture.finish_process(&ticket).map_err(io)?;
            return Err(io(error));
        }
    };
    let pid = child
        .id()
        .ok_or_else(|| issue(IssueCode::RecoveryRequired, "Git process has no identity"))?;
    if let Err(error) = capture.register_process(&ticket, pid) {
        let stopped = child.stop().await;
        if stopped.is_ok() {
            capture.finish_process(&ticket).map_err(io)?;
        }
        return Err(io(format!(
            "Register Git process: {error}; stop: {stopped:?}"
        )));
    }
    let mut stdout = child
        .stdout()
        .ok_or_else(|| corrupt("Git stdout was not captured"))?;
    let mut stderr = child
        .stderr()
        .ok_or_else(|| corrupt("Git stderr was not captured"))?;
    let out = pipe(&mut stdout, capture, OutputStream::Stdout);
    let err = pipe(&mut stderr, capture, OutputStream::Stderr);
    let wait = async {
        loop {
            tokio::select! {
                status = child.wait() => return status.map_err(io),
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if service.clone_cancelled(request)? {
                        child.stop().await.map_err(io)?;
                        return Err(issue(IssueCode::Busy, "Checkout clone was cancelled"));
                    }
                }
            }
        }
    };
    let (outcome, stdout_result, stderr_result) = tokio::join!(wait, out, err);
    capture.finish_process(&ticket).map_err(io)?;
    stdout_result?;
    stderr_result?;
    let status = outcome?;
    if !status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            format!(
                "Git operation failed ({status}); retained execution output contains diagnostics. No checkout was published"
            ),
        ));
    }
    Ok(())
}

async fn pipe(
    source: &mut (impl tokio::io::AsyncRead + Unpin),
    capture: &dyn OutputCapture,
    stream: OutputStream,
) -> Result<()> {
    let mut bytes = [0u8; 8192];
    let mut failure = None;
    loop {
        let count = source.read(&mut bytes).await.map_err(io)?;
        if count == 0 {
            break;
        }
        // Drain even after a storage failure so the owned child can terminate.
        if failure.is_none()
            && let Err(error) = capture.write(stream, &bytes[..count])
        {
            failure = Some(io(error));
        }
    }
    if let Some(error) = failure {
        Err(error)
    } else {
        Ok(())
    }
}
