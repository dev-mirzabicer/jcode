//! Client-local external editor handoff. No remote shell or authoritative path
//! is ever passed to the child process.
use super::EditorRequest;
use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, EventStream,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Serialize, Deserialize)]
struct LocalDraftRecord {
    draft: String,
    file: String,
    generation: u64,
    initial_sha256: String,
}

pub(crate) fn editor_command(
    visual: Option<&str>,
    editor: Option<&str>,
    path: &Path,
) -> Result<Command> {
    let selected = visual
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor.filter(|value| !value.trim().is_empty()))
        .unwrap_or("nano");
    let words = shlex::split(selected).context("Editor command has unmatched quotes")?;
    let (program, arguments) = words.split_first().context("Editor command is empty")?;
    anyhow::ensure!(!program.is_empty(), "Editor executable is empty");
    let mut command = Command::new(program);
    command.args(arguments).arg(path);
    Ok(command)
}

pub(crate) fn prepare(directory: &Path, request: &EditorRequest) -> Result<PathBuf> {
    crate::storage::ensure_dir(directory)?;
    crate::platform::set_directory_permissions_owner_only(directory)?;
    let identity = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", request.draft, request.file).as_bytes())
    );
    let path = directory.join(format!("{identity}.md"));
    let receipt = directory.join(format!("{identity}.json"));
    let sha = format!("{:x}", Sha256::digest(request.body.as_bytes()));
    if path.exists() || path.is_symlink() {
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Retained draft is not a regular file: {}",
            path.display()
        );
        let body = std::fs::read_to_string(&path).context("Read retained local editor draft")?;
        if body != request.body {
            let record: LocalDraftRecord = serde_json::from_slice(
                &std::fs::read(&receipt).context("Read local draft recovery metadata")?,
            )?;
            anyhow::ensure!(
                record.draft == request.draft
                    && record.file == request.file
                    && record.generation == request.generation
                    && record.initial_sha256 == sha,
                "Local and server drafts have diverged. Retained local text: {}. Compare it with the current draft before recovering it; neither was overwritten.",
                path.display()
            );
        } else {
            crate::storage::write_json_secret(
                &receipt,
                &LocalDraftRecord {
                    draft: request.draft.clone(),
                    file: request.file.clone(),
                    generation: request.generation,
                    initial_sha256: sha,
                },
            )?;
        }
        return Ok(path);
    }
    let mut file = tempfile::Builder::new()
        .prefix("editor-")
        .suffix(".md")
        .tempfile_in(directory)?;
    file.write_all(request.body.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path)
        .context("Publish private editor draft")?;
    crate::platform::set_permissions_owner_only(&path)?;
    crate::storage::write_json_secret(
        &receipt,
        &LocalDraftRecord {
            draft: request.draft.clone(),
            file: request.file.clone(),
            generation: request.generation,
            initial_sha256: sha,
        },
    )?;
    Ok(path)
}

pub(crate) fn run(
    terminal: &mut DefaultTerminal,
    events: &mut Option<EventStream>,
    directory: &Path,
    request: &EditorRequest,
) -> Result<(PathBuf, Result<String>)> {
    let path = prepare(directory, request)?;
    let mut command = editor_command(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
        &path,
    )?;
    // Drop wakes Crossterm's pending poll task. Acquire/release the reader once
    // before launching the child so it can no longer consume editor input.
    drop(events.take());
    let _ = crossterm::event::poll(std::time::Duration::ZERO);
    let mut errors = Vec::new();
    macro_rules! attempt {
        ($label:literal, $operation:expr) => {
            if let Err(error) = $operation {
                errors.push(format!("{}: {error}", $label));
            }
        };
    }
    attempt!(
        "disable paste",
        crossterm::execute!(std::io::stdout(), DisableBracketedPaste)
    );
    attempt!(
        "disable focus",
        crossterm::execute!(std::io::stdout(), DisableFocusChange)
    );
    attempt!(
        "disable mouse",
        crossterm::execute!(std::io::stdout(), DisableMouseCapture)
    );
    crate::tui::disable_keyboard_enhancement();
    attempt!("restore cooked input", disable_raw_mode());
    attempt!(
        "leave alternate screen",
        crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)
    );
    attempt!("show cursor", crossterm::execute!(std::io::stdout(), Show));
    let child = if errors.is_empty() {
        command.status().context("Launch external editor")
    } else {
        Err(anyhow::anyhow!(
            "Editor was not launched because terminal handoff failed"
        ))
    };
    attempt!("reenter raw mode", enable_raw_mode());
    attempt!(
        "reenter alternate screen",
        crossterm::execute!(std::io::stdout(), EnterAlternateScreen)
    );
    attempt!(
        "enable paste",
        crossterm::execute!(std::io::stdout(), EnableBracketedPaste)
    );
    let policy = crate::perf::tui_policy();
    if policy.enable_focus_change {
        attempt!(
            "enable focus",
            crossterm::execute!(std::io::stdout(), EnableFocusChange)
        );
    }
    if policy.enable_mouse_capture {
        attempt!(
            "enable mouse",
            crossterm::execute!(std::io::stdout(), EnableMouseCapture)
        );
    }
    if policy.enable_keyboard_enhancement {
        crate::tui::enable_keyboard_enhancement();
    }
    attempt!("clear restored terminal", terminal.clear());
    *events = Some(EventStream::new());
    let result = finish(&path, child, errors);
    Ok((path, result))
}

fn finish(path: &Path, child: Result<ExitStatus>, terminal_errors: Vec<String>) -> Result<String> {
    let mut errors = terminal_errors;
    match child {
        Ok(status) if !status.success() => errors.push(format!("Editor exited with {status}")),
        Err(error) => errors.push(format!("{error:#}")),
        _ => {}
    }
    anyhow::ensure!(
        errors.is_empty(),
        "{}\nOriginal source is unchanged. Local draft retained at {}",
        errors.join("\n"),
        path.display()
    );
    let metadata = std::fs::symlink_metadata(path).context("Inspect completed editor draft")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Editor output is not a regular file. Source is unchanged; inspect {}",
        path.display()
    );
    std::fs::read_to_string(path)
        .with_context(|| format!("Read completed editor draft {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_arguments_are_quoted_not_shell_evaluated() {
        let command = editor_command(
            Some("'/editor path' --wait 'two words'"),
            Some("ignored"),
            Path::new("/draft path.md"),
        )
        .unwrap();
        assert_eq!(command.get_program(), "/editor path");
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["--wait", "two words", "/draft path.md"]
        );
        assert!(editor_command(Some("'unclosed"), None, Path::new("draft")).is_err());
    }
    #[test]
    fn complete_editor_drafts_preserve_failed_local_edits_and_reject_divergence() {
        let root = tempfile::tempdir().unwrap();
        let request = EditorRequest {
            draft: "opaque".into(),
            generation: 1,
            file: "modules/source.md".into(),
            body: "初期 {{literal}}".repeat(100_000),
            repair: false,
            metadata_field: None,
        };
        let path = prepare(root.path(), &request).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), request.body);
        std::fs::write(&path, "retained local text").unwrap();
        assert_eq!(prepare(root.path(), &request).unwrap(), path);
        let changed = EditorRequest {
            generation: 2,
            body: "different server version".into(),
            ..request
        };
        assert!(prepare(root.path(), &changed).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "retained local text"
        );
        // The server acknowledged this version. A second editor failure must
        // retain edits relative to the new generation, not the old receipt.
        std::fs::write(&path, &changed.body).unwrap();
        prepare(root.path(), &changed).unwrap();
        std::fs::write(&path, "second retained edit").unwrap();
        assert_eq!(prepare(root.path(), &changed).unwrap(), path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "second retained edit"
        );
    }
    #[cfg(unix)]
    #[test]
    fn editor_exit_and_spawn_errors_retain_the_draft_and_all_terminal_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("draft.md");
        std::fs::write(&path, "draft").unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .status()
            .map_err(anyhow::Error::from);
        let error = finish(&path, child, vec!["restore sentinel".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("7") && error.contains("restore sentinel"));
        assert!(
            finish(
                &path,
                Command::new(root.path().join("absent-editor"))
                    .status()
                    .map_err(anyhow::Error::from),
                Vec::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "draft");
    }
}
