use super::types::*;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const OPERATION_TRAILER: &str = "Jcode-Instruction-Operation";

#[derive(Clone, Debug)]
pub(super) struct GitRepository {
    root: PathBuf,
    binding: InstructionRepositoryResult<GitBinding>,
}

#[derive(Clone, Debug)]
struct GitBinding {
    work_tree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    script_policy: Vec<OsString>,
}

#[derive(Clone, Debug)]
pub(super) struct GitTreeEntry {
    pub(super) object_id: String,
    pub(super) mode: String,
    pub(super) path: PathBuf,
}

impl GitRepository {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let binding = bind_git(&root);
        Self { root, binding }
    }

    pub(super) fn is_repository(&self) -> bool {
        self.binding.is_ok()
    }

    pub(super) fn common_directory(&self) -> Option<&Path> {
        self.binding
            .as_ref()
            .ok()
            .map(|binding| binding.common_dir.as_path())
    }

    pub(super) fn require_no_pending_transaction(&self) -> InstructionRepositoryResult<()> {
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        for name in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
        ] {
            match std::fs::symlink_metadata(binding.git_dir.join(name)) {
                Ok(_) => return Err(InstructionRepositoryError::new(InstructionRepositoryErrorKind::Conflict, "check Git transaction", "A merge, rebase or sequencer operation is still pending. Finish or abort it explicitly with Git before an instruction Save. No files were overwritten.").path(&self.root)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(InstructionRepositoryError::new(InstructionRepositoryErrorKind::Io, "inspect Git transaction", error.to_string()).path(&self.root)),
            }
        }
        if self.changes()?.iter().any(|change| change.conflicted) {
            return Err(InstructionRepositoryError::new(InstructionRepositoryErrorKind::Conflict, "check Git index", "The instruction repository has unresolved index entries. Resolve them explicitly with Git before Save.").path(&self.root));
        }
        Ok(())
    }

    pub(super) fn head(&self) -> InstructionRepositoryResult<Option<String>> {
        let output = self.run(["rev-parse", "--verify", "HEAD"])?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(
            utf8_stdout("inspect HEAD", output)?.trim().to_string(),
        ))
    }

    pub(super) fn branch(&self) -> InstructionRepositoryResult<Option<String>> {
        let output = self.run(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(
            utf8_stdout("inspect branch", output)?.trim().to_string(),
        ))
    }

    pub(super) fn upstream(
        &self,
    ) -> InstructionRepositoryResult<Option<InstructionRepositoryUpstream>> {
        let output = self.run([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])?;
        if !output.status.success() {
            return Ok(None);
        }
        let reference = utf8_stdout("inspect upstream", output)?.trim().to_string();
        let counts = self.checked_utf8(
            "inspect ahead and behind",
            ["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )?;
        let mut parts = counts.split_whitespace();
        let ahead = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let behind = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let (remote, branch) = reference
            .split_once('/')
            .map_or((None, None), |(remote, branch)| {
                (Some(remote.to_string()), Some(branch.to_string()))
            });
        Ok(Some(InstructionRepositoryUpstream {
            reference,
            remote,
            branch,
            ahead,
            behind,
        }))
    }

    pub(super) fn references(&self) -> InstructionRepositoryResult<String> {
        self.checked_utf8(
            "inspect reference identities",
            [
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
                "refs/remotes",
            ],
        )
    }

    pub(super) fn branch_names(&self, remote: bool) -> InstructionRepositoryResult<Vec<String>> {
        let values = self.checked_utf8(
            "list branches",
            [
                "for-each-ref",
                "--format=%(refname:short)",
                if remote { "refs/remotes" } else { "refs/heads" },
            ],
        )?;
        Ok(values
            .lines()
            .filter(|line| !line.ends_with("/HEAD"))
            .map(str::to_string)
            .collect())
    }

    pub(super) fn remotes(&self) -> InstructionRepositoryResult<Vec<(String, String)>> {
        self.checked_utf8("list remotes", ["remote"])?
            .lines()
            .map(|name| Ok((name.to_string(), self.remote_url(name)?.unwrap_or_default())))
            .collect()
    }

    pub(super) fn resolve_reference(&self, reference: &str) -> InstructionRepositoryResult<String> {
        if reference.starts_with('-') || reference.chars().any(char::is_control) {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "resolve Git reference",
                "Invalid reference",
            ));
        }
        let value = self.checked_utf8(
            "resolve Git reference",
            ["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        )?;
        let value = value.trim().to_string();
        validate_commit_id(&value)?;
        Ok(value)
    }

    pub(super) fn outgoing(
        &self,
        remote: &str,
        branch: &str,
    ) -> InstructionRepositoryResult<Vec<String>> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        let local = self.resolve_reference(&format!("refs/heads/{branch}"))?;
        let range = match self.resolve_reference(&format!("refs/remotes/{remote}/{branch}")) {
            Ok(remote) => format!("{remote}..{local}"),
            Err(_) => local,
        };
        Ok(self
            .checked_utf8(
                "inspect outgoing commits",
                ["log", "--format=%H %s", &range],
            )?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub(super) fn push_snapshot(
        &self,
        remote: &str,
        branch: &str,
        commit: &str,
    ) -> InstructionRepositoryResult<()> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        validate_commit_id(commit)?;
        self.checked(
            "push reviewed snapshot",
            [
                "-c",
                "push.followTags=false",
                "push",
                "--recurse-submodules=no",
                "--",
                remote,
                &format!("{commit}:refs/heads/{branch}"),
            ],
        )?;
        Ok(())
    }

    pub(super) fn index_digest(&self) -> InstructionRepositoryResult<Option<String>> {
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        match std::fs::read(binding.git_dir.join("index")) {
            Ok(bytes) => Ok(Some(super::mutation::sha256(&bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "inspect Git index",
                error.to_string(),
            )),
        }
    }

    pub(super) fn remote_url(&self, remote: &str) -> InstructionRepositoryResult<Option<String>> {
        validate_remote(remote)?;
        let output = self.run(["remote", "get-url", "--", remote])?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(
            utf8_stdout("inspect remote URL", output)?
                .trim()
                .to_string(),
        ))
    }

    pub(super) fn changes(&self) -> InstructionRepositoryResult<Vec<InstructionRepositoryChange>> {
        let output = self.checked_bytes(
            "inspect working tree",
            ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )?;
        parse_porcelain_v2(&output)
    }

    pub(super) fn show_file(
        &self,
        commit: &str,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<Option<Vec<u8>>> {
        validate_commit_id(commit)?;
        let path = git_path(relative_path)?;
        let spec = format!("{commit}:{path}");
        let output = self.run([
            OsStr::new("cat-file"),
            OsStr::new("blob"),
            OsStr::new(&spec),
        ])?;
        if output.status.success() {
            Ok(Some(output.stdout))
        } else if stderr_contains_missing_object(&output.stderr) {
            Ok(None)
        } else {
            Err(git_failure("read content at revision", &self.root, output))
        }
    }

    pub(super) fn file_exists_at_head(
        &self,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<bool> {
        let Some(head) = self.head()? else {
            return Ok(false);
        };
        Ok(self.show_file(&head, relative_path)?.is_some())
    }

    /// Read exact blobs, not an archive/checkout that can apply attributes.
    /// File-backed input/output avoids pipe deadlocks and buffering the whole
    /// repository in RAM. Both files are private temporary snapshot artifacts.
    pub(super) fn materialize_blobs(
        &self,
        entries: &[GitTreeEntry],
        directory: &Path,
    ) -> InstructionRepositoryResult<()> {
        let io_error = |operation: &str, error: std::io::Error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                operation,
                error.to_string(),
            )
            .path(directory)
        };
        let malformed = || {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::GitCommand,
                "read committed blob batch",
                "Git returned an incomplete or mismatched blob batch",
            )
            .path(&self.root)
        };
        let mut input = tempfile::tempfile_in(directory)
            .map_err(|error| io_error("create blob query", error))?;
        let mut output_file = tempfile::tempfile_in(directory)
            .map_err(|error| io_error("create blob capture", error))?;
        for entry in entries {
            super::mutation::validate_relative_path(&entry.path)?;
            validate_commit_id(&entry.object_id)?;
            if !matches!(entry.mode.as_str(), "100644" | "100755") {
                return Err(malformed());
            }
            writeln!(input, "{}", entry.object_id)
                .map_err(|error| io_error("write blob query", error))?;
        }
        if entries.is_empty() {
            return Ok(());
        }
        input
            .rewind()
            .map_err(|error| io_error("rewind blob query", error))?;
        let capture = output_file
            .try_clone()
            .map_err(|error| io_error("open blob capture", error))?;
        let result = self
            .command_with_env(
                ["cat-file", "--batch"],
                std::iter::empty::<(&OsStr, &OsStr)>(),
            )?
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(capture))
            .output()
            .map_err(|error| io_error("read Git blob batch", error))?;
        if !result.status.success() {
            return Err(git_failure("read committed blob batch", &self.root, result));
        }
        output_file
            .rewind()
            .map_err(|error| io_error("rewind blob capture", error))?;
        let mut reader = BufReader::new(output_file);
        for entry in entries {
            let mut header = String::new();
            reader
                .read_line(&mut header)
                .map_err(|error| io_error("read blob header", error))?;
            let fields = header.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 || fields[0] != entry.object_id || fields[1] != "blob" {
                return Err(malformed());
            }
            let size: u64 = fields[2].parse().map_err(|_| malformed())?;
            let target = directory.join(&entry.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| io_error("create snapshot directory", error))?;
            }
            let mut file = std::fs::File::create(&target)
                .map_err(|error| io_error("create snapshot file", error))?;
            let copied = std::io::copy(&mut reader.by_ref().take(size), &mut file)
                .map_err(|error| io_error("copy complete committed blob", error))?;
            let mut delimiter = [0u8];
            if copied != size || reader.read_exact(&mut delimiter).is_err() || delimiter != *b"\n" {
                return Err(malformed());
            }
        }
        let mut trailing = [0u8];
        if reader
            .read(&mut trailing)
            .map_err(|error| io_error("finish blob capture", error))?
            != 0
        {
            return Err(malformed());
        }
        Ok(())
    }

    pub(super) fn file_existed_in_history(
        &self,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<bool> {
        let history = self.checked_utf8(
            "inspect resource history",
            [
                OsStr::new("rev-list"),
                OsStr::new("-1"),
                OsStr::new("HEAD"),
                OsStr::new("--"),
                relative_path.as_os_str(),
            ],
        )?;
        Ok(!history.trim().is_empty())
    }

    pub(super) fn history(
        &self,
        relative_path: Option<&Path>,
    ) -> InstructionRepositoryResult<Vec<InstructionHistoryEntry>> {
        self.history_page(relative_path, "HEAD", 0, None)
    }

    pub(super) fn history_page(
        &self,
        relative_path: Option<&Path>,
        revision: &str,
        offset: usize,
        limit: Option<usize>,
    ) -> InstructionRepositoryResult<Vec<InstructionHistoryEntry>> {
        if revision != "HEAD" {
            validate_commit_id(revision)?;
        }
        let mut args = vec![OsString::from("rev-list"), OsString::from(revision)];
        args.push(format!("--skip={offset}").into());
        if let Some(limit) = limit {
            args.push(format!("--max-count={limit}").into());
        }
        if let Some(path) = relative_path {
            args.push(OsString::from("--"));
            args.push(path.as_os_str().to_os_string());
        }
        let commits = self.checked_utf8_os("list history", &args)?;
        let mut entries = Vec::new();
        for commit in commits.lines().filter(|line| !line.is_empty()) {
            validate_commit_id(commit)?;
            let metadata = self.checked_bytes(
                "read history metadata",
                [
                    "show",
                    "-s",
                    "--format=%H%x00%P%x00%an%x00%ae%x00%aI%x00%s",
                    commit,
                ],
            )?;
            let fields = metadata.split(|byte| *byte == 0).collect::<Vec<_>>();
            if fields.len() < 6 {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::GitCommand,
                    "parse history",
                    format!("Git returned malformed metadata for commit {commit}"),
                )
                .path(&self.root));
            }
            let changed = self.checked_bytes(
                "read history paths",
                [
                    "diff-tree",
                    "--root",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "-z",
                    commit,
                ],
            )?;
            let changed_paths = changed
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(path_from_git_bytes)
                .collect();
            entries.push(InstructionHistoryEntry {
                commit: utf8_field("history commit", fields[0])?,
                parents: utf8_field("history parents", fields[1])?
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                author_name: utf8_field("history author", fields[2])?,
                author_email: utf8_field("history email", fields[3])?,
                authored_at: utf8_field("history time", fields[4])?,
                subject: utf8_field("history subject", fields[5])?
                    .trim_end()
                    .to_string(),
                changed_paths,
            });
        }
        Ok(entries)
    }

    pub(super) fn tree_entries(
        &self,
        commit: &str,
    ) -> InstructionRepositoryResult<Vec<GitTreeEntry>> {
        validate_commit_id(commit)?;
        let bytes = self.checked_bytes(
            "list instruction tree",
            ["ls-tree", "-r", "-z", "--full-tree", commit],
        )?;
        bytes
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| {
                let (metadata, path) = record.split_once_byte(b'\t').ok_or_else(|| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::GitCommand,
                        "parse instruction tree",
                        format!(
                            "Git returned malformed tree entry: {}",
                            String::from_utf8_lossy(record)
                        ),
                    )
                })?;
                let mode = metadata.split(|byte| *byte == b' ').next().ok_or_else(|| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::GitCommand,
                        "parse instruction tree",
                        "Git tree entry has no mode",
                    )
                })?;
                let object_id = metadata.split(|byte| *byte == b' ').nth(2).ok_or_else(|| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::GitCommand,
                        "parse instruction tree",
                        "Git tree entry has no object identity",
                    )
                })?;
                let object_id = utf8_field("tree object", object_id)?;
                validate_commit_id(&object_id)?;
                Ok(GitTreeEntry {
                    object_id,
                    mode: utf8_field("tree mode", mode)?,
                    path: path_from_git_bytes(path),
                })
            })
            .collect()
    }

    pub(super) fn compare(
        &self,
        from: &str,
        to: &str,
        relative_path: Option<&Path>,
    ) -> InstructionRepositoryResult<String> {
        validate_commit_id(from)?;
        validate_commit_id(to)?;
        let mut args = vec![
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--no-textconv"),
            OsString::from("--no-color"),
            OsString::from(from),
            OsString::from(to),
        ];
        if let Some(path) = relative_path {
            args.push(OsString::from("--"));
            args.push(path.as_os_str().to_os_string());
        }
        self.checked_utf8_os("compare revisions", &args)
    }

    pub(super) fn working_diff(&self, path: Option<&Path>) -> InstructionRepositoryResult<String> {
        let mut args: Vec<OsString> = [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "HEAD",
            "--",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if let Some(path) = path {
            args.push(path.as_os_str().into());
        }
        self.checked_utf8_os("inspect working diff", &args)
    }

    pub(super) fn enclosing_root(path: &Path) -> InstructionRepositoryResult<Option<PathBuf>> {
        let directory = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        if !directory.exists() {
            return Ok(None);
        }
        let output = run_git(
            Some(directory),
            ["rev-parse", "--show-toplevel"],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(
            utf8_stdout("inspect enclosing repository", output)?.trim(),
        )))
    }

    pub(super) fn find_operation_commit(
        &self,
        operation_id: &str,
    ) -> InstructionRepositoryResult<Option<String>> {
        validate_operation_id(operation_id)?;
        let pattern = format!("{OPERATION_TRAILER}: {operation_id}");
        let output = self.run([
            "log",
            "--all",
            "--fixed-strings",
            "--grep",
            &pattern,
            "--format=%H",
        ])?;
        if !output.status.success() {
            return Err(git_failure("find completed operation", &self.root, output));
        }
        // Git's fixed-string grep still matches substrings. An operation is a
        // complete trailer value, not a prefix or incidental subject text.
        for commit in utf8_stdout("find completed operation", output)?.lines() {
            validate_commit_id(commit)?;
            let body = self.checked_utf8(
                "read operation identity",
                ["show", "-s", "--format=%B", commit],
            )?;
            if body.lines().rev().find(|line| !line.trim().is_empty()) == Some(pattern.as_str()) {
                return Ok(Some(commit.to_string()));
            }
        }
        Ok(None)
    }

    pub(super) fn commit_paths(
        &self,
        index_path: &Path,
        paths: &[PathBuf],
        subject: &str,
        operation_id: &str,
        expected_head: &str,
    ) -> InstructionRepositoryResult<Option<String>> {
        validate_operation_id(operation_id)?;
        validate_commit_id(expected_head)?;
        let branch = self.branch()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::DetachedHead,
                "commit",
                "Save requires an attached instruction-repository branch",
            )
            .path(&self.root)
        })?;
        let index_value = index_path.as_os_str();
        self.checked_with_env(
            "prepare isolated index",
            ["read-tree", expected_head],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;

        self.stage_raw_paths(index_path, paths)?;

        let diff = self.run_with_env(
            ["diff", "--cached", "--quiet", "--exit-code"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        match diff.status.code() {
            Some(0) => return Ok(None),
            Some(1) => {}
            _ => return Err(git_failure("compare isolated index", &self.root, diff)),
        }

        let tree = self.checked_utf8_with_env(
            "write instruction tree",
            ["write-tree"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        let tree = tree.trim();
        let message = format!(
            "{}\n\n{OPERATION_TRAILER}: {operation_id}\n",
            normalized_subject(subject)
        );
        let mut author_env = Vec::new();
        if !self.author_is_configured() {
            author_env.extend([
                (OsString::from("GIT_AUTHOR_NAME"), OsString::from("Jcode")),
                (
                    OsString::from("GIT_AUTHOR_EMAIL"),
                    OsString::from("jcode@localhost"),
                ),
                (
                    OsString::from("GIT_COMMITTER_NAME"),
                    OsString::from("Jcode"),
                ),
                (
                    OsString::from("GIT_COMMITTER_EMAIL"),
                    OsString::from("jcode@localhost"),
                ),
            ]);
        }
        let commit = self.checked_utf8_os_env_owned(
            "create instruction commit",
            &[
                OsString::from("commit-tree"),
                OsString::from(tree),
                OsString::from("-p"),
                OsString::from(expected_head),
            ],
            &author_env,
            Some(message.as_bytes()),
        )?;
        let commit = commit.trim().to_string();
        validate_commit_id(&commit)?;
        let reference = format!("refs/heads/{branch}");
        self.checked(
            "publish instruction commit",
            ["update-ref", &reference, &commit, expected_head],
        )?;
        self.refresh_index_paths(paths)?;
        Ok(Some(commit))
    }

    fn stage_raw_paths(
        &self,
        index_path: &Path,
        paths: &[PathBuf],
    ) -> InstructionRepositoryResult<()> {
        let index = [(OsStr::new("GIT_INDEX_FILE"), index_path.as_os_str())];
        for path in paths {
            super::mutation::validate_relative_path(path)?;
            let target = self.root.join(path);
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let mut file = match options.open(&target) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.checked_with_env(
                        "stage instruction deletion",
                        [
                            OsStr::new("update-index"),
                            OsStr::new("--force-remove"),
                            OsStr::new("--"),
                            path.as_os_str(),
                        ],
                        index,
                        None,
                    )?;
                    continue;
                }
                Err(error) => {
                    return Err(InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Io,
                        "stage instruction bytes",
                        error.to_string(),
                    )
                    .path(&target));
                }
            };
            let metadata = file.metadata().map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "inspect staged file",
                    error.to_string(),
                )
                .path(&target)
            })?;
            if !metadata.is_file() {
                return Err(InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::InvalidPath,
                    "stage instruction bytes",
                    "Only regular files can be committed",
                )
                .path(&target));
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "read staged bytes",
                    error.to_string(),
                )
                .path(&target)
            })?;
            let blob = self.checked_utf8_with_env(
                "write exact instruction blob",
                ["hash-object", "-w", "--stdin"],
                index,
                Some(&bytes),
            )?;
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            let mode = if executable { "100755" } else { "100644" };
            let cache_info = format!("{mode},{},{}", blob.trim(), git_path(path)?);
            self.checked_with_env(
                "stage exact instruction blob",
                ["update-index", "--add", "--cacheinfo", &cache_info],
                index,
                None,
            )?;
        }
        Ok(())
    }

    pub(super) fn initial_commit(
        &self,
        index_path: &Path,
        paths: &[PathBuf],
        subject: &str,
        operation_id: &str,
    ) -> InstructionRepositoryResult<String> {
        validate_operation_id(operation_id)?;
        let branch = self.branch()?.unwrap_or_else(|| "main".to_string());
        let index_value = index_path.as_os_str();
        self.checked_with_env(
            "prepare initial isolated index",
            ["read-tree", "--empty"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        self.stage_raw_paths(index_path, paths)?;
        let tree = self.checked_utf8_with_env(
            "write initial instruction tree",
            ["write-tree"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        let message = format!(
            "{}\n\n{OPERATION_TRAILER}: {operation_id}\n",
            normalized_subject(subject)
        );
        let commit = self.create_commit(tree.trim(), None, message.as_bytes())?;
        self.checked_with_env(
            "publish initial instruction commit",
            ["update-ref", "--stdin"],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            Some(format!("create refs/heads/{branch} {commit}\n").as_bytes()),
        )?;
        self.refresh_index_paths(paths)?;
        Ok(commit)
    }

    pub(super) fn commit_virtual_files(
        &self,
        index_path: &Path,
        writes: &[(PathBuf, Vec<u8>)],
        deletes: &[PathBuf],
        subject: &str,
        operation_id: &str,
        expected_head: &str,
    ) -> InstructionRepositoryResult<Option<String>> {
        self.require_no_pending_transaction()?;
        validate_operation_id(operation_id)?;
        validate_commit_id(expected_head)?;
        let branch = self.branch()?.ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::DetachedHead,
                "commit virtual instruction files",
                "Save requires an attached instruction-repository branch",
            )
            .path(&self.root)
        })?;
        let index_value = index_path.as_os_str();
        self.checked_with_env(
            "prepare isolated index",
            ["read-tree", expected_head],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        for (path, content) in writes {
            let blob = self.checked_utf8_with_env(
                "write instruction blob",
                ["hash-object", "-w", "--stdin"],
                [(OsStr::new("GIT_INDEX_FILE"), index_value)],
                Some(content),
            )?;
            let cache_info = format!("100644,{},{}", blob.trim(), git_path(path)?);
            self.checked_with_env(
                "stage instruction blob",
                ["update-index", "--add", "--cacheinfo", &cache_info],
                [(OsStr::new("GIT_INDEX_FILE"), index_value)],
                None,
            )?;
        }
        for path in deletes {
            let path = git_path(path)?;
            self.checked_with_env(
                "stage instruction deletion",
                ["update-index", "--force-remove", "--", &path],
                [(OsStr::new("GIT_INDEX_FILE"), index_value)],
                None,
            )?;
        }
        let diff = self.run_with_env(
            ["diff", "--cached", "--quiet", "--exit-code"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        match diff.status.code() {
            Some(0) => return Ok(None),
            Some(1) => {}
            _ => return Err(git_failure("compare isolated index", &self.root, diff)),
        }
        let tree = self.checked_utf8_with_env(
            "write instruction tree",
            ["write-tree"],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
        let message = format!(
            "{}\n\n{OPERATION_TRAILER}: {operation_id}\n",
            normalized_subject(subject)
        );
        let commit = self.create_commit(tree.trim(), Some(expected_head), message.as_bytes())?;
        self.checked(
            "publish instruction commit",
            [
                "update-ref",
                &format!("refs/heads/{branch}"),
                &commit,
                expected_head,
            ],
        )?;
        let paths = writes
            .iter()
            .map(|(path, _)| path.clone())
            .chain(deletes.iter().cloned())
            .collect::<Vec<_>>();
        self.refresh_index_paths(&paths)?;
        Ok(Some(commit))
    }

    pub(super) fn init(root: &Path, branch: &str) -> InstructionRepositoryResult<Self> {
        validate_branch(branch)?;
        let output = run_git(
            Some(root),
            ["init", "--initial-branch", branch],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            return Err(git_failure("initialize", root, output));
        }
        Ok(Self::new(root))
    }

    pub(super) fn clone_remote(
        url: &str,
        branch: &str,
        destination: &Path,
    ) -> InstructionRepositoryResult<Self> {
        validate_branch(branch)?;
        if url.trim().is_empty() || url.contains(['\n', '\r', '\0']) {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "clone external repository",
                "repository URL is empty or contains a control character",
            ));
        }
        let parent = destination.parent().ok_or_else(|| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "clone external repository",
                "checkout path has no parent directory",
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "create external checkout parent",
                error.to_string(),
            )
            .path(parent)
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".jcode-clone-")
            .tempdir_in(parent)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::Io,
                    "prepare private clone",
                    error.to_string(),
                )
                .path(parent)
            })?;
        let checkout = staging.path().join("checkout");
        let output = run_git(
            Some(parent),
            [
                OsStr::new("clone"),
                OsStr::new("--branch"),
                OsStr::new(branch),
                OsStr::new("--single-branch"),
                OsStr::new("--"),
                OsStr::new(url),
                checkout.as_os_str(),
            ],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            let mut error = git_failure("clone external repository", destination, output);
            let path = staging.path().to_path_buf();
            if let Err(cleanup) = staging.close() {
                error.detail.push_str(&format!(
                    "; staging cleanup failed at {}: {cleanup}",
                    path.display()
                ));
            }
            return Err(error);
        }
        let candidate = Self::new(&checkout);
        if !candidate.is_repository()
            || candidate.head()?.is_none()
            || candidate.branch()?.as_deref() != Some(branch)
        {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::RepositoryDamaged,
                "validate cloned checkout",
                "Clone did not produce the requested attached branch and complete Git worktree",
            )
            .path(destination));
        }
        if destination.exists() || destination.is_symlink() {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Conflict,
                "publish cloned checkout",
                "Destination appeared during clone; it was not overwritten",
            )
            .path(destination));
        }
        std::fs::rename(&checkout, destination).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "publish cloned checkout",
                error.to_string(),
            )
            .path(destination)
        })?;
        let staging_path = staging.path().to_path_buf();
        staging.close().map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "clean completed clone staging",
                format!(
                    "Checkout is published at {}. Cleanup remains at {}: {error}",
                    destination.display(),
                    staging_path.display()
                ),
            )
            .path(destination)
            .may_have_working_changes()
        })?;
        Ok(Self::new(destination))
    }

    pub(super) fn branch_checkout(
        &self,
        branch: &str,
        create: bool,
        start: Option<&str>,
    ) -> InstructionRepositoryResult<()> {
        validate_branch(branch)?;
        let mut args = vec![OsString::from("switch")];
        if create {
            args.push(OsString::from("--create"));
            args.push(OsString::from(branch));
            if let Some(start) = start {
                if start.contains(['\n', '\r', '\0']) || start.starts_with('-') {
                    return Err(InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Configuration,
                        "create branch",
                        "invalid branch start point",
                    ));
                }
                args.push(OsString::from(start));
            }
        } else {
            args.extend([OsString::from("--"), OsString::from(branch)]);
        }
        self.checked_os("change branch", &args)?;
        Ok(())
    }

    pub(super) fn fetch(&self, remote: &str) -> InstructionRepositoryResult<()> {
        validate_remote(remote)?;
        self.checked("fetch", ["fetch", "--prune", "--", remote])?;
        Ok(())
    }

    pub(super) fn fetch_branch(
        &self,
        remote: &str,
        branch: &str,
    ) -> InstructionRepositoryResult<()> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        self.checked(
            "fetch selected remote branch",
            [
                "fetch",
                "--",
                remote,
                &format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}"),
            ],
        )?;
        Ok(())
    }

    pub(super) fn pull(
        &self,
        remote: &str,
        branch: &str,
        strategy: InstructionPullStrategy,
    ) -> InstructionRepositoryResult<()> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        let strategy_flag = match strategy {
            InstructionPullStrategy::FastForwardOnly => "--ff-only",
            InstructionPullStrategy::Merge => "--no-ff",
        };
        self.checked_with_env(
            "pull",
            [
                "pull",
                "--no-rebase",
                "--no-edit",
                strategy_flag,
                "--",
                remote,
                branch,
            ],
            [(OsStr::new("GIT_EDITOR"), OsStr::new("true"))],
            None,
        )?;
        Ok(())
    }

    pub(super) fn push(
        &self,
        remote: &str,
        branch: &str,
        set_upstream: bool,
    ) -> InstructionRepositoryResult<()> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        let mut args = vec![OsString::from("push")];
        if set_upstream {
            args.push(OsString::from("--set-upstream"));
        }
        args.extend([
            OsString::from("--"),
            OsString::from(remote),
            OsString::from(branch),
        ]);
        self.checked_os("push", &args)?;
        Ok(())
    }

    pub(super) fn set_remote(&self, name: &str, url: &str) -> InstructionRepositoryResult<()> {
        validate_remote(name)?;
        if url.trim().is_empty() || url.contains(['\n', '\r', '\0']) {
            return Err(InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Configuration,
                "configure remote",
                "repository URL is empty or contains a control character",
            ));
        }
        let exists = self.run(["remote", "get-url", name])?.status.success();
        if exists {
            self.checked("configure remote", ["remote", "set-url", name, url])?;
        } else {
            self.checked("configure remote", ["remote", "add", name, url])?;
        }
        Ok(())
    }

    pub(super) fn add_submodule(
        parent: &Path,
        url: &str,
        branch: &str,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<Self> {
        validate_branch(branch)?;
        let path = git_path(relative_path)?;
        let output = run_git(
            Some(parent),
            [
                OsStr::new("-c"),
                OsStr::new("protocol.file.allow=always"),
                OsStr::new("submodule"),
                OsStr::new("add"),
                OsStr::new("--branch"),
                OsStr::new(branch),
                OsStr::new("--"),
                OsStr::new(url),
                OsStr::new(&path),
            ],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            return Err(git_failure("add instruction submodule", parent, output));
        }
        Ok(Self::new(parent.join(relative_path)))
    }

    pub(super) fn submodule_metadata(
        parent: &Path,
        path: &Path,
    ) -> InstructionRepositoryResult<Option<PathBuf>> {
        let git = Self::new(parent);
        let mapping = git.run([
            "config",
            "--null",
            "--file",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ])?;
        if !mapping.status.success() {
            return Ok(None);
        }
        for record in utf8_stdout("read submodule metadata", mapping)?.split('\0') {
            if let Some((key, value)) = record.split_once('\n')
                && value == git_path(path)?
            {
                let Some(name) = key
                    .strip_prefix("submodule.")
                    .and_then(|key| key.strip_suffix(".path"))
                else {
                    continue;
                };
                super::mutation::validate_relative_path(Path::new(name))?;
                let location = git.checked_utf8(
                    "resolve submodule Git directory",
                    [
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-path",
                        &format!("modules/{name}"),
                    ],
                )?;
                let location = PathBuf::from(location.trim_end());
                return Ok(location.is_dir().then_some(location));
            }
        }
        Ok(None)
    }

    pub(super) fn restore_submodule(parent: &Path, path: &Path) -> InstructionRepositoryResult<()> {
        let git = Self::new(parent);
        let mut args = match Self::submodule_metadata(parent, path)? {
            Some(metadata) => script_disabling_config_at(parent, Some(&metadata))?,
            None => Vec::new(),
        };
        args.extend([
            OsString::from("-c"),
            OsString::from("protocol.file.allow=always"),
            OsString::from("submodule"),
            OsString::from("update"),
            OsString::from("--init"),
            OsString::from("--checkout"),
            OsString::from("--"),
            path.as_os_str().to_os_string(),
        ]);
        git.checked_os("restore missing submodule checkout", &args)
            .map_err(InstructionRepositoryError::may_have_working_changes)?;
        Ok(())
    }

    pub(super) fn configured_submodule_url(
        parent: &Path,
        path: &Path,
    ) -> InstructionRepositoryResult<Option<String>> {
        let git = Self::new(parent);
        let mapping = git.run([
            "config",
            "--null",
            "--file",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ])?;
        if !mapping.status.success() {
            return Ok(None);
        }
        let mapping = utf8_stdout("read submodule paths", mapping)?;
        for record in mapping.split('\0') {
            if let Some((key, value)) = record.split_once('\n')
                && value == git_path(path)?
            {
                let key = format!("{}.url", key.strip_suffix(".path").unwrap_or(key));
                let output = git.run(["config", "--get", &key])?;
                if output.status.success() {
                    return Ok(Some(
                        utf8_stdout("read resolved submodule URL", output)?
                            .trim_end()
                            .to_string(),
                    ));
                }
            }
        }
        Ok(None)
    }

    pub(super) fn submodule_recorded_commit(
        parent: &Path,
        relative_path: &Path,
    ) -> InstructionRepositoryResult<Option<String>> {
        let path = git_path(relative_path)?;
        let output = run_git(
            Some(parent),
            ["ls-files", "--stage", "--", &path],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            return Err(git_failure("inspect parent gitlink", parent, output));
        }
        let stdout = utf8_stdout("inspect parent gitlink", output)?;
        let Some(line) = stdout.lines().next() else {
            return Ok(None);
        };
        let mut fields = line.split_whitespace();
        let mode = fields.next();
        let commit = fields.next();
        Ok((mode == Some("160000")).then(|| commit.unwrap_or_default().to_string()))
    }

    fn author_is_configured(&self) -> bool {
        self.run(["var", "GIT_AUTHOR_IDENT"])
            .is_ok_and(|output| output.status.success())
    }

    pub(super) fn refresh_index_paths(&self, paths: &[PathBuf]) -> InstructionRepositoryResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            OsString::from("reset"),
            OsString::from("--quiet"),
            OsString::from("HEAD"),
            OsString::from("--"),
        ];
        args.extend(paths.iter().map(|path| path.as_os_str().to_os_string()));
        self.checked_os("refresh committed instruction index paths", &args)?;
        Ok(())
    }

    fn create_commit(
        &self,
        tree: &str,
        parent: Option<&str>,
        message: &[u8],
    ) -> InstructionRepositoryResult<String> {
        let mut args = vec![OsString::from("commit-tree"), OsString::from(tree)];
        if let Some(parent) = parent {
            args.extend([OsString::from("-p"), OsString::from(parent)]);
        }
        let mut author_env = Vec::new();
        if !self.author_is_configured() {
            author_env.extend([
                (OsString::from("GIT_AUTHOR_NAME"), OsString::from("Jcode")),
                (
                    OsString::from("GIT_AUTHOR_EMAIL"),
                    OsString::from("jcode@localhost"),
                ),
                (
                    OsString::from("GIT_COMMITTER_NAME"),
                    OsString::from("Jcode"),
                ),
                (
                    OsString::from("GIT_COMMITTER_EMAIL"),
                    OsString::from("jcode@localhost"),
                ),
            ]);
        }
        let commit = self.checked_utf8_os_env_owned(
            "create instruction commit",
            &args,
            &author_env,
            Some(message),
        )?;
        let commit = commit.trim().to_string();
        validate_commit_id(&commit)?;
        Ok(commit)
    }

    fn checked<I, S>(&self, operation: &str, args: I) -> InstructionRepositoryResult<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(git_failure(operation, &self.root, output))
        }
    }

    fn checked_os(
        &self,
        operation: &str,
        args: &[OsString],
    ) -> InstructionRepositoryResult<Output> {
        let output = self.run(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(git_failure(operation, &self.root, output))
        }
    }

    fn checked_utf8<I, S>(&self, operation: &str, args: I) -> InstructionRepositoryResult<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        utf8_stdout(operation, self.checked(operation, args)?)
    }

    fn checked_bytes<I, S>(&self, operation: &str, args: I) -> InstructionRepositoryResult<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(self.checked(operation, args)?.stdout)
    }

    fn checked_utf8_os(
        &self,
        operation: &str,
        args: &[OsString],
    ) -> InstructionRepositoryResult<String> {
        utf8_stdout(operation, self.checked_os(operation, args)?)
    }

    fn checked_with_env<I, S, E, K, V>(
        &self,
        operation: &str,
        args: I,
        env: E,
        stdin: Option<&[u8]>,
    ) -> InstructionRepositoryResult<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let output = self.run_with_env(args, env, stdin)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(git_failure(operation, &self.root, output))
        }
    }

    fn checked_utf8_with_env<I, S, E, K, V>(
        &self,
        operation: &str,
        args: I,
        env: E,
        stdin: Option<&[u8]>,
    ) -> InstructionRepositoryResult<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        utf8_stdout(
            operation,
            self.checked_with_env(operation, args, env, stdin)?,
        )
    }

    fn checked_utf8_os_env_owned(
        &self,
        operation: &str,
        args: &[OsString],
        env: &[(OsString, OsString)],
        stdin: Option<&[u8]>,
    ) -> InstructionRepositoryResult<String> {
        utf8_stdout(
            operation,
            self.checked_with_env(
                operation,
                args,
                env.iter()
                    .map(|(key, value)| (key.as_os_str(), value.as_os_str())),
                stdin,
            )?,
        )
    }

    fn run<I, S>(&self, args: I) -> InstructionRepositoryResult<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_with_env(args, std::iter::empty::<(&OsStr, &OsStr)>(), None)
    }

    fn run_with_env<I, S, E, K, V>(
        &self,
        args: I,
        env: E,
        stdin: Option<&[u8]>,
    ) -> InstructionRepositoryResult<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let command = self.command_with_env(args, env)?;
        execute_command(command, stdin, Some(&self.root))
    }

    fn command_with_env<I, S, E, K, V>(
        &self,
        args: I,
        env: E,
    ) -> InstructionRepositoryResult<Command>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let mut environment = vec![
            (
                OsString::from("GIT_DIR"),
                binding.git_dir.as_os_str().to_os_string(),
            ),
            (
                OsString::from("GIT_WORK_TREE"),
                binding.work_tree.as_os_str().to_os_string(),
            ),
        ];
        environment.extend(
            env.into_iter()
                .map(|(key, value)| (key.as_ref().to_os_string(), value.as_ref().to_os_string())),
        );
        Ok(git_command(
            Some(&binding.work_tree),
            args,
            environment,
            &binding.script_policy,
        ))
    }
}

fn bind_git(root: &Path) -> InstructionRepositoryResult<GitBinding> {
    let fail = |detail: String| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::RepositoryDamaged,
            "bind instruction Git worktree",
            detail,
        )
        .path(root)
    };
    if !root.is_dir() || root.is_symlink() {
        return Err(fail(
            "Instruction checkout root must not be a symlink".into(),
        ));
    }
    let script_policy = script_disabling_config(root)?;
    let output = execute_git(
        Some(root),
        [
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--path-format=absolute",
            "--git-common-dir",
        ],
        std::iter::empty::<(&OsStr, &OsStr)>(),
        None,
        &script_policy,
    )?;
    if !output.status.success() {
        return Err(git_failure("bind instruction Git worktree", root, output));
    }
    let output = utf8_stdout("bind instruction Git worktree", output)?;
    let paths = output.lines().collect::<Vec<_>>();
    if paths.len() != 3 {
        return Err(fail("Git returned an ambiguous worktree identity".into()));
    }
    let canonical = |path: &Path| path.canonicalize().map_err(|error| fail(error.to_string()));
    let work_tree = canonical(Path::new(paths[0]))?;
    if work_tree != canonical(root)? {
        return Err(fail("Configured instruction directory is not the Git worktree root. Refusing to adopt or mutate its parent repository.".into()));
    }
    Ok(GitBinding {
        work_tree,
        git_dir: canonical(Path::new(paths[1]))?,
        common_dir: canonical(Path::new(paths[2]))?,
        script_policy,
    })
}

fn script_disabling_config(root: &Path) -> InstructionRepositoryResult<Vec<OsString>> {
    script_disabling_config_at(root, None)
}
fn script_disabling_config_at(
    root: &Path,
    git_dir: Option<&Path>,
) -> InstructionRepositoryResult<Vec<OsString>> {
    let mut arguments = Vec::new();
    if let Some(git_dir) = git_dir {
        arguments.extend([
            OsString::from("--git-dir"),
            git_dir.as_os_str().to_os_string(),
            OsString::from("--work-tree"),
            root.as_os_str().to_os_string(),
        ]);
    }
    arguments.extend(
        [
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^(filter\..*\.(clean|smudge|process|required)|merge\..*\.driver)$",
        ]
        .into_iter()
        .map(OsString::from),
    );
    let output = execute_git(
        Some(root),
        arguments,
        std::iter::empty::<(&OsStr, &OsStr)>(),
        None,
        &[],
    )?;
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err(git_failure("inspect Git execution policy", root, output));
    }
    let keys = utf8_stdout("inspect Git execution policy", output)?;
    let mut policy = Vec::new();
    for key in keys.split('\0').filter(|key| !key.is_empty()) {
        policy.push(OsString::from("-c"));
        let value = if key.ends_with(".required") || key.starts_with("merge.") {
            "false"
        } else {
            ""
        };
        policy.push(format!("{key}={value}").into());
    }
    Ok(policy)
}

fn run_git<I, S, E, K, V>(
    current_dir: Option<&Path>,
    args: I,
    env: E,
    stdin: Option<&[u8]>,
) -> InstructionRepositoryResult<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let policy = match current_dir {
        Some(root) => script_disabling_config(root)?,
        None => Vec::new(),
    };
    execute_git(current_dir, args, env, stdin, &policy)
}

fn execute_git<I, S, E, K, V>(
    current_dir: Option<&Path>,
    args: I,
    env: E,
    stdin: Option<&[u8]>,
    script_policy: &[OsString],
) -> InstructionRepositoryResult<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    execute_command(
        git_command(current_dir, args, env, script_policy),
        stdin,
        current_dir,
    )
}

fn git_command<I, S, E, K, V>(
    current_dir: Option<&Path>,
    args: I,
    env: E,
    script_policy: &[OsString],
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            if cfg!(windows) {
                "core.hooksPath=NUL"
            } else {
                "core.hooksPath=/dev/null"
            },
            "-c",
            "submodule.recurse=false",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "core.autocrlf=false",
        ])
        .args(script_policy)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

fn execute_command(
    mut command: Command,
    stdin: Option<&[u8]>,
    current_dir: Option<&Path>,
) -> InstructionRepositoryResult<Output> {
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::GitCommand,
            "launch Git",
            error.to_string(),
        )
        .path(current_dir.unwrap_or_else(|| Path::new(".")))
    })?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::GitCommand,
                    "write Git input",
                    "Git stdin pipe was unavailable",
                )
            })?
            .write_all(input)
            .map_err(|error| {
                InstructionRepositoryError::new(
                    InstructionRepositoryErrorKind::GitCommand,
                    "write Git input",
                    error.to_string(),
                )
            })?;
    }
    child.wait_with_output().map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::GitCommand,
            "wait for Git",
            error.to_string(),
        )
        .path(current_dir.unwrap_or_else(|| Path::new(".")))
    })
}

fn redact_git_urls(value: &str) -> String {
    static URLS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = URLS.get_or_init(|| {
        regex::Regex::new(r#"(?:https?|ssh)://[^\s'"<>]+"#).expect("static URL pattern")
    });
    pattern
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let original = &captures[0];
            let Ok(mut url) = url::Url::parse(original) else {
                return "[invalid URL hidden]".to_string();
            };
            let _ = url.set_password(None);
            if matches!(url.scheme(), "http" | "https") {
                let _ = url.set_username("");
            }
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        })
        .into_owned()
}

fn git_failure(operation: &str, root: &Path, output: Output) -> InstructionRepositoryError {
    let stderr = redact_git_urls(String::from_utf8_lossy(&output.stderr).trim());
    let detail = if stderr.is_empty() {
        format!("Git exited with status {}", output.status)
    } else {
        stderr
    };
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::GitCommand,
        operation,
        detail,
    )
    .path(root)
}

fn parse_porcelain_v2(
    bytes: &[u8],
) -> InstructionRepositoryResult<Vec<InstructionRepositoryChange>> {
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        match record[0] {
            b'1' => {
                let fields = split_fields(record, 9)?;
                let (index_delta, worktree_delta, conflicted) = parse_xy(fields[1])?;
                changes.push(InstructionRepositoryChange {
                    path: path_from_git_bytes(fields[8]),
                    original_path: None,
                    index: index_delta,
                    worktree: worktree_delta,
                    conflicted,
                });
            }
            b'2' => {
                let fields = split_fields(record, 10)?;
                let original = records.get(index).ok_or_else(|| malformed_status(record))?;
                index += 1;
                let (index_delta, worktree_delta, conflicted) = parse_xy(fields[1])?;
                changes.push(InstructionRepositoryChange {
                    path: path_from_git_bytes(fields[9]),
                    original_path: Some(path_from_git_bytes(original)),
                    index: index_delta,
                    worktree: worktree_delta,
                    conflicted,
                });
            }
            b'u' => {
                let fields = split_fields(record, 11)?;
                changes.push(InstructionRepositoryChange {
                    path: path_from_git_bytes(fields[10]),
                    original_path: None,
                    index: Some(GitDelta::Unmerged),
                    worktree: Some(GitDelta::Unmerged),
                    conflicted: true,
                });
            }
            b'?' => changes.push(InstructionRepositoryChange {
                path: path_from_git_bytes(record.get(2..).unwrap_or_default()),
                original_path: None,
                index: None,
                worktree: Some(GitDelta::Untracked),
                conflicted: false,
            }),
            b'!' => {}
            _ => return Err(malformed_status(record)),
        }
    }
    Ok(changes)
}

fn split_fields(record: &[u8], count: usize) -> InstructionRepositoryResult<Vec<&[u8]>> {
    let fields = record
        .splitn(count, |byte| *byte == b' ')
        .collect::<Vec<_>>();
    if fields.len() == count {
        Ok(fields)
    } else {
        Err(malformed_status(record))
    }
}

fn parse_xy(
    bytes: &[u8],
) -> InstructionRepositoryResult<(Option<GitDelta>, Option<GitDelta>, bool)> {
    if bytes.len() != 2 {
        return Err(malformed_status(bytes));
    }
    let index = delta(bytes[0]);
    let worktree = delta(bytes[1]);
    let conflicted = bytes.contains(&b'U');
    Ok((index, worktree, conflicted))
}

fn delta(byte: u8) -> Option<GitDelta> {
    match byte {
        b'.' | b' ' => None,
        b'A' => Some(GitDelta::Added),
        b'M' => Some(GitDelta::Modified),
        b'D' => Some(GitDelta::Deleted),
        b'R' => Some(GitDelta::Renamed),
        b'C' => Some(GitDelta::Copied),
        b'T' => Some(GitDelta::TypeChanged),
        b'U' => Some(GitDelta::Unmerged),
        b'?' => Some(GitDelta::Untracked),
        _ => Some(GitDelta::Unknown),
    }
}

fn malformed_status(record: &[u8]) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::GitCommand,
        "parse working-tree state",
        format!(
            "Git returned malformed porcelain-v2 record: {}",
            String::from_utf8_lossy(record)
        ),
    )
}

fn utf8_stdout(operation: &str, output: Output) -> InstructionRepositoryResult<String> {
    String::from_utf8(output.stdout).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidUtf8,
            operation,
            format!("Git output was not UTF-8: {error}"),
        )
    })
}

fn utf8_field(label: &str, bytes: &[u8]) -> InstructionRepositoryResult<String> {
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidUtf8,
            "parse Git history",
            format!("{label} was not UTF-8: {error}"),
        )
    })
}

fn validate_commit_id(commit: &str) -> InstructionRepositoryResult<()> {
    if matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate commit",
            "commit identity must be a complete hexadecimal object ID",
        ))
    }
}

pub(super) fn validate_operation_id(operation_id: &str) -> InstructionRepositoryResult<()> {
    let valid = !operation_id.is_empty()
        && operation_id.len() <= 128
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate operation identity",
            "operation ID must contain only ASCII letters, digits, '-', '_', or '.'",
        ))
    }
}

pub(super) fn validate_branch(branch: &str) -> InstructionRepositoryResult<()> {
    if branch.is_empty()
        || branch == "HEAD"
        || branch.contains(['\n', '\r', '\0'])
        || branch.starts_with('-')
    {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate branch",
            "invalid branch name",
        ));
    }
    let output = run_git(
        None,
        ["check-ref-format", &format!("refs/heads/{branch}")],
        std::iter::empty::<(&OsStr, &OsStr)>(),
        None,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate branch",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn validate_remote(remote: &str) -> InstructionRepositoryResult<()> {
    let valid = !remote.is_empty()
        && remote.len() <= 256
        && !remote.starts_with('-')
        && remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate remote",
            "invalid remote name",
        ))
    }
}

pub(super) fn git_path(path: &Path) -> InstructionRepositoryResult<String> {
    let value = path.to_str().ok_or_else(|| {
        InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidUtf8,
            "encode Git path",
            "managed instruction paths must be valid UTF-8",
        )
        .path(path)
    })?;
    if value.is_empty() || value.contains(['\n', '\r', '\0', ':']) || value.starts_with('-') {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::InvalidPath,
            "encode Git path",
            "managed path is empty or contains a Git-ambiguous character",
        )
        .path(path));
    }
    Ok(value.replace('\\', "/"))
}

#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn normalized_subject(subject: &str) -> String {
    let subject = subject.lines().next().unwrap_or_default().trim();
    if subject.is_empty() {
        "instruction: update managed resources".to_string()
    } else {
        subject.to_string()
    }
}

fn stderr_contains_missing_object(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("does not exist")
        || stderr.contains("Not a valid object name")
        || stderr.contains("path '") && stderr.contains("exists on disk, but not in")
}

trait SplitOnceByte {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])>;
}

impl SplitOnceByte for [u8] {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])> {
        let index = self.iter().position(|candidate| *candidate == byte)?;
        Some((&self[..index], &self[index + 1..]))
    }
}
