use super::types::*;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const OPERATION_TRAILER: &str = "Jcode-Instruction-Operation";

#[derive(Clone, Debug)]
pub(super) struct GitRepository {
    root: PathBuf,
}

impl GitRepository {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(super) fn is_repository(&self) -> bool {
        self.run(["rev-parse", "--is-inside-work-tree"])
            .is_ok_and(|output| output.status.success() && trim_ascii(&output.stdout) == b"true")
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

    pub(super) fn history(
        &self,
        relative_path: Option<&Path>,
    ) -> InstructionRepositoryResult<Vec<InstructionHistoryEntry>> {
        let mut args = vec![OsString::from("rev-list"), OsString::from("HEAD")];
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
            "-n",
            "1",
        ])?;
        if !output.status.success() {
            return Err(git_failure("find completed operation", &self.root, output));
        }
        let commit = utf8_stdout("find completed operation", output)?;
        let commit = commit.trim();
        Ok((!commit.is_empty()).then(|| commit.to_string()))
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

        let mut add_args = vec![
            OsString::from("add"),
            OsString::from("-A"),
            OsString::from("--"),
        ];
        add_args.extend(paths.iter().map(|path| path.as_os_str().to_os_string()));
        self.checked_os_with_env(
            "stage owned instruction paths",
            &add_args,
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;

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

    pub(super) fn initial_commit(
        &self,
        index_path: &Path,
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
        self.checked_with_env(
            "stage initial instruction store",
            ["add", "-A", "--", "."],
            [(OsStr::new("GIT_INDEX_FILE"), index_value)],
            None,
        )?;
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
        self.checked(
            "publish initial instruction commit",
            ["update-ref", &format!("refs/heads/{branch}"), &commit],
        )?;
        self.checked(
            "initialize working index",
            ["reset", "--mixed", "--quiet", "HEAD"],
        )?;
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
        let output = run_git(
            Some(parent),
            [
                OsStr::new("clone"),
                OsStr::new("--branch"),
                OsStr::new(branch),
                OsStr::new("--single-branch"),
                OsStr::new("--"),
                OsStr::new(url),
                destination.as_os_str(),
            ],
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )?;
        if !output.status.success() {
            return Err(git_failure(
                "clone external repository",
                destination,
                output,
            ));
        }
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

    fn checked_os_with_env<E, K, V>(
        &self,
        operation: &str,
        args: &[OsString],
        env: E,
        stdin: Option<&[u8]>,
    ) -> InstructionRepositoryResult<Output>
    where
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.checked_with_env(operation, args, env, stdin)
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
        run_git(
            Some(&self.root),
            args,
            std::iter::empty::<(&OsStr, &OsStr)>(),
            None,
        )
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
        run_git(Some(&self.root), args, env, stdin)
    }
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
    let mut command = Command::new("git");
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }
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

fn git_failure(operation: &str, root: &Path, output: Output) -> InstructionRepositoryError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
    if branch.is_empty() || branch.contains(['\n', '\r', '\0']) || branch.starts_with('-') {
        return Err(InstructionRepositoryError::new(
            InstructionRepositoryErrorKind::Configuration,
            "validate branch",
            "invalid branch name",
        ));
    }
    let output = run_git(
        None,
        ["check-ref-format", "--branch", branch],
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

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}
