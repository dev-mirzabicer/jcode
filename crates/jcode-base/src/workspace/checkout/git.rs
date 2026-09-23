use super::*;
use crate::execution::owned_child::OwnedChild;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::ffi::OsStr;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
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
                // ls-remote's trailing arguments filter *names*, not object IDs.
                // A peeled annotated tag is a commit; its tag object is not.
                let output = authorized(None, [OsStr::new("ls-remote"), OsStr::new(url)], false)?
                    .output()
                    .map_err(io)?;
                let lines = parse_ls_remote(output)?;
                let annotated_tags: std::collections::HashSet<_> = lines
                    .iter()
                    .filter_map(|(_, name)| name.strip_suffix("^{}"))
                    .collect();
                let tag_objects: std::collections::HashSet<_> = lines
                    .iter()
                    .filter(|(_, name)| annotated_tags.contains(name.as_str()))
                    .map(|(oid, _)| oid.as_str())
                    .collect();
                if !lines.iter().any(|(oid, name)| {
                    oid == &reference
                        && !tag_objects.contains(oid.as_str())
                        && (name == "HEAD"
                            || name.starts_with("refs/heads/")
                            || name.ends_with("^{}")
                            || name.starts_with("refs/tags/"))
                }) {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Commit is not advertised by this source; select a branch or tag, or an existing local checkout",
                    ));
                }
                reference
            } else {
                let peeled = format!("{reference}^{{}}");
                let output = authorized(
                    None,
                    [
                        OsStr::new("ls-remote"),
                        OsStr::new(url),
                        OsStr::new(&reference),
                        OsStr::new(&peeled),
                    ],
                    false,
                )?
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
/// redirection, global filters or SSH overrides. Network operations separately
/// select user/system credential settings without inheriting unrelated config.
pub(in crate::workspace) fn git<I, S>(cwd: Option<&Path>, arguments: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command(cwd, arguments, false)
}

/// Explicit LFS work uses repository-local filters installed without hooks.
/// Ordinary acquisition keeps smudge/process disabled until requested content
/// is fully fetched, verified and ready for publication.
pub(super) fn git_lfs<I, S>(cwd: Option<&Path>, arguments: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command(cwd, arguments, true)
}

/// Only an explicitly selected Git transport may use the user's configured
/// credential helpers. The selected config remains process-local, never in a
/// clone review, catalog receipt, invocation input or command-line argument.
pub(super) fn authorized<I, S>(cwd: Option<&Path>, arguments: I, lfs: bool) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = command(cwd, arguments, lfs);
    let mut selected = vec![("credential.helper".to_owned(), String::new())];
    selected.extend(configured_credentials()?);
    command.env("GIT_CONFIG_COUNT", selected.len().to_string());
    for (index, (key, value)) in selected.into_iter().enumerate() {
        command.env(format!("GIT_CONFIG_KEY_{index}"), key);
        command.env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    command
        .env("GIT_ASKPASS", "/usr/bin/false")
        .env("SSH_ASKPASS", "/usr/bin/false")
        .env("GCM_INTERACTIVE", "never");
    Ok(command)
}

fn configured_credentials() -> Result<Vec<(String, String)>> {
    let directory = tempfile::tempdir().map_err(io)?;
    let mut query = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            query.env_remove(key);
        }
    }
    // A caller may disable system configuration. It cannot redirect the
    // trusted user/system config paths through ambient GIT_CONFIG_* values.
    if std::env::var("GIT_CONFIG_NOSYSTEM").is_ok_and(|v| v == "1") {
        query.env("GIT_CONFIG_NOSYSTEM", "1");
    }
    let output = query
        .current_dir(directory.path())
        .args(["config", "--includes", "--null", "--list", "--show-scope"])
        .stdin(Stdio::null())
        .output()
        .map_err(io)?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Trusted Git credential configuration is unreadable; repair it before retrying the source",
        ));
    }
    select_credential_settings(&output.stdout)
}

fn select_credential_settings(source: &[u8]) -> Result<Vec<(String, String)>> {
    if !source.is_empty() && source.last() != Some(&0) {
        return Err(corrupt("Truncated scoped Git configuration"));
    }
    let entries = source.split(|byte| *byte == 0).collect::<Vec<_>>();
    let entries = &entries[..entries.len().saturating_sub(1)];
    if entries.len() % 2 != 0 || entries.iter().any(|entry| entry.is_empty()) {
        return Err(corrupt("Malformed scoped Git configuration"));
    }
    let mut selected = Vec::new();
    for record in entries.chunks_exact(2) {
        let scope = std::str::from_utf8(record[0]).map_err(io)?;
        if !matches!(scope, "global" | "system") {
            continue;
        }
        let split = record[1]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| corrupt("Malformed Git credential setting"))?;
        let (key, value) = record[1].split_at(split);
        let value = &value[1..];
        let key = std::str::from_utf8(key).map_err(io)?;
        let lower = key.to_ascii_lowercase();
        if lower.starts_with("credential.")
            && [".helper", ".username", ".usehttppath"]
                .iter()
                .any(|suffix| lower.ends_with(suffix))
        {
            selected.push((
                key.to_owned(),
                String::from_utf8(value.to_vec()).map_err(io)?,
            ));
        }
    }
    Ok(selected)
}

/// Git's submodule helper may use local hardlinks even when the enclosing
/// checkout was cloned with --no-local. Replace only aliases within our
/// witnessed, unpublished stage. This preserves every object and ref while
/// removing a source inode dependency without modifying the source repository.
pub(super) fn detach_borrowed_objects(stage: &Path, checkout: &Path) -> Result<()> {
    let stage = stage.canonicalize().map_err(io)?;
    let checkout = checkout.canonicalize().map_err(io)?;
    if !checkout.starts_with(&stage) {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Git checkout escaped the owned clone stage",
        ));
    }
    let response = git(Some(&checkout), ["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(io)?;
    if !response.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone Git directory is unavailable",
        ));
    }
    let text = std::str::from_utf8(&response.stdout).map_err(io)?.trim();
    let git_dir = Path::new(text).canonicalize().map_err(io)?;
    if !git_dir.starts_with(&stage) {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone borrows Git metadata outside its own stage",
        ));
    }
    let objects = git_dir.join("objects");
    if objects.join("info/alternates").try_exists().map_err(io)? {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone borrows source Git objects through alternates",
        ));
    }
    detach_tree(&objects)?;
    let lfs = git_dir.join("lfs/objects");
    if lfs.try_exists().map_err(io)? {
        detach_tree(&lfs)?;
    }
    Ok(())
}

fn detach_tree(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if !std::fs::symlink_metadata(&directory)
            .map_err(io)?
            .file_type()
            .is_dir()
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Git object directory is not an ordinary directory",
            ));
        }
        for entry in std::fs::read_dir(&directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(io)?;
            if metadata.file_type().is_dir() {
                pending.push(path);
            } else if metadata.file_type().is_file() {
                detach_file(&path, &metadata)?;
            } else {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Clone object store contains an unexpected link or special file",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn detach_file(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if metadata.nlink() <= 1 {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| corrupt("Git object has no parent"))?;
    let mut original = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    let initial = original.metadata().map_err(io)?;
    if initial.dev() != metadata.dev()
        || initial.ino() != metadata.ino()
        || initial.len() != metadata.len()
    {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone object changed before detaching a borrowed inode",
        ));
    }
    let mut owned = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    let copied = std::io::copy(&mut original, owned.as_file_mut()).map_err(io)?;
    if copied != initial.len() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone object changed while copying an independent copy",
        ));
    }
    owned
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(io)?;
    owned.as_file().sync_all().map_err(io)?;
    let current = std::fs::symlink_metadata(path).map_err(io)?;
    if current.dev() != initial.dev()
        || current.ino() != initial.ino()
        || current.len() != initial.len()
    {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone object was replaced before independent publication",
        ));
    }
    owned.persist(path).map_err(io)?;
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(io)?;
    if std::fs::symlink_metadata(path).map_err(io)?.nlink() != 1 {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone object is still hardlinked after detachment",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn detach_file(_path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    Err(issue(
        IssueCode::UnsupportedCapability,
        "Independent native Git object detachment is unavailable on this platform",
    ))
}

fn command<I, S>(cwd: Option<&Path>, arguments: I, materializing_lfs: bool) -> Command
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
        ]);
    if !materializing_lfs {
        command.env("GIT_LFS_SKIP_SMUDGE", "1").args([
            "-c",
            "filter.lfs.smudge=",
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.required=false",
        ]);
    }
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

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn only_trusted_scopes_and_selected_credential_keys_cross_git_config_boundary() {
        let raw = b"local\0credential.helper\n/fixture/untrusted-repo-helper\0\
                    global\0credential.helper\n/fixture/user-helper\0\
                    global\0credential.http://fixture.invalid.helper\n/fixture/scoped-helper\0\
                    system\0credential.helper\nosxkeychain\0\
                    global\0credential.http://fixture.invalid.usehttppath\ntrue\0\
                    global\0filter.lfs.process\n/fixture/untrusted-filter\0\
                    global\0core.hookspath\n/fixture/untrusted-hook\0\
                    global\0credential.password\nsynthetic-private-data\0";
        assert_eq!(
            select_credential_settings(raw).unwrap(),
            vec![
                ("credential.helper".into(), "/fixture/user-helper".into()),
                (
                    "credential.http://fixture.invalid.helper".into(),
                    "/fixture/scoped-helper".into()
                ),
                ("credential.helper".into(), "osxkeychain".into()),
                (
                    "credential.http://fixture.invalid.usehttppath".into(),
                    "true".into()
                ),
            ]
        );
        assert_eq!(select_credential_settings(b"").unwrap(), Vec::new());
        assert_eq!(
            select_credential_settings(b"global\0credential.helper\n\0").unwrap(),
            vec![("credential.helper".into(), String::new())]
        );
        assert_eq!(
            select_credential_settings(b"global\0credential.helper\nmissing terminal record")
                .unwrap_err()
                .code,
            IssueCode::CorruptState
        );
    }
}
