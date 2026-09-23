use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

impl WorkspaceService {
    pub(super) async fn materialize_clone(
        &self,
        request: RequestId,
        operation: &CloneOperation,
        stage: &PhysicalBinding,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let spec = &operation.public.review.spec;
        let root = stage.observed_path();
        let oid = &operation.public.review.source_commit;
        self.verify_acquired(operation, stage)?;
        self.git_step(
            request,
            root,
            [
                OsStr::new("checkout"),
                OsStr::new("--detach"),
                OsStr::new(oid),
            ],
            capture,
        )
        .await?;
        let head = checked_git(root, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
        if head.trim() != oid {
            return Err(issue(
                IssueCode::Conflict,
                "Checkout HEAD differs from reviewed commit",
            ));
        }
        // An interrupted stage is not a scratch directory that may be reset.
        // A branch whose name already exists at another commit blocks rather than being forced.
        match &spec.branch {
            CloneBranch::KeepName => {
                let CloneBase::Branch { name } = &spec.base else {
                    return Err(corrupt("Invalid reviewed branch selection"));
                };
                self.select_branch(request, root, name, oid, capture)
                    .await?;
            }
            CloneBranch::Create { name } => {
                self.select_branch(request, root, name, oid, capture)
                    .await?
            }
            CloneBranch::Detached => {}
        }
        if spec.submodules {
            let mut visited = HashSet::new();
            self.materialize_submodules(request, root, root, spec, capture, &mut visited)
                .await?;
        }
        if spec.lfs {
            self.materialize_lfs_tree(request, root, root, spec, capture)
                .await?;
        }
        self.git_step(
            request,
            root,
            [
                OsStr::new("remote"),
                OsStr::new("remove"),
                OsStr::new("origin"),
            ],
            capture,
        )
        .await?;
        for remote in &spec.remotes {
            self.git_step(
                request,
                root,
                [
                    OsStr::new("remote"),
                    OsStr::new("add"),
                    OsStr::new(&remote.name),
                    OsStr::new(&remote.url),
                ],
                capture,
            )
            .await?;
        }
        verify_clean_tree(root, spec)?;
        self.check_cancel(request)?;
        Ok(())
    }

    async fn select_branch(
        &self,
        request: RequestId,
        root: &Path,
        name: &str,
        oid: &str,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        git::validate_branch(name)?;
        let branch = format!("refs/heads/{name}");
        let previous = git::git(
            Some(root),
            [
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new(&branch),
            ],
        )
        .output()
        .map_err(io)?;
        if previous.status.success() {
            if std::str::from_utf8(&previous.stdout).map_err(io)?.trim() != oid {
                return Err(issue(
                    IssueCode::Conflict,
                    "Selected branch already exists at another commit in the stage",
                ));
            }
            self.git_step(
                request,
                root,
                [OsStr::new("switch"), OsStr::new(name)],
                capture,
            )
            .await
        } else {
            self.git_step(
                request,
                root,
                [
                    OsStr::new("switch"),
                    OsStr::new("-c"),
                    OsStr::new(name),
                    OsStr::new(oid),
                ],
                capture,
            )
            .await
        }
    }

    async fn git_step<I, S>(
        &self,
        request: RequestId,
        root: &Path,
        args: I,
        capture: &dyn OutputCapture,
    ) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.check_cancel(request)?;
        git::run(self, request, git::git(Some(root), args), capture).await
    }

    async fn lfs_step<I, S>(
        &self,
        request: RequestId,
        root: &Path,
        args: I,
        capture: &dyn OutputCapture,
    ) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.check_cancel(request)?;
        git::run(self, request, git::git_lfs(Some(root), args), capture).await
    }

    async fn materialize_submodules(
        &self,
        request: RequestId,
        project_root: &Path,
        root: &Path,
        spec: &CloneSpec,
        capture: &dyn OutputCapture,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        let mut pending = vec![root.to_path_buf()];
        while let Some(parent) = pending.pop() {
            let parent = parent.canonicalize().map_err(io)?;
            if !parent.starts_with(project_root) || !visited.insert(parent.clone()) {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Submodule path escaped or repeated a repository",
                ));
            }
            for (name, relative) in submodules(&parent)? {
                let candidate = parent.join(&relative);
                if !candidate.starts_with(project_root) {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Submodule path escaped the selected clone",
                    ));
                }
                let url = checked_git(
                    &parent,
                    [
                        OsStr::new("config"),
                        OsStr::new("--file"),
                        OsStr::new(".gitmodules"),
                        OsStr::new("--get"),
                        OsStr::new(&format!("submodule.{name}.url")),
                    ],
                )?;
                let url = url.trim();
                let local = url.starts_with("../")
                    || url.starts_with("./")
                    || url.starts_with("file://")
                    || url.starts_with('/');
                if local
                    && !spec
                        .trusted_local_submodule_urls
                        .iter()
                        .any(|approved| approved == url)
                {
                    return Err(issue(
                        IssueCode::PermissionRequired,
                        format!(
                            "Submodule {name} uses a local/relative transport not approved in the clone review"
                        ),
                    ));
                }
                if !local {
                    checked_git_url(url)?;
                }
                let mut command = git::git(
                    Some(&parent),
                    [
                        OsStr::new("-c"),
                        OsStr::new(if local {
                            "protocol.file.allow=always"
                        } else {
                            "protocol.file.allow=never"
                        }),
                        OsStr::new("submodule"),
                        OsStr::new("update"),
                        OsStr::new("--init"),
                        OsStr::new("--checkout"),
                        OsStr::new("--"),
                        relative.as_os_str(),
                    ],
                );
                command.env("GIT_PROTOCOL_FROM_USER", "0");
                git::run(self, request, command, capture).await?;
                let actual = candidate.canonicalize().map_err(io)?;
                if !actual.starts_with(&parent) || !actual.starts_with(project_root) {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Submodule resolved outside the checked-out repository",
                    ));
                }
                git::detach_borrowed_objects(project_root, &actual)?;
                pending.push(actual);
            }
        }
        Ok(())
    }

    async fn materialize_lfs_tree(
        &self,
        request: RequestId,
        project_root: &Path,
        root: &Path,
        spec: &CloneSpec,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let mut pending = vec![root.to_path_buf()];
        while let Some(parent) = pending.pop() {
            let config = parent.join(".lfsconfig");
            if config.exists() {
                let output = git::git(
                    Some(&parent),
                    [
                        OsStr::new("config"),
                        OsStr::new("--file"),
                        OsStr::new(".lfsconfig"),
                        OsStr::new("--get"),
                        OsStr::new("lfs.url"),
                    ],
                )
                .output()
                .map_err(io)?;
                if output.status.success() {
                    let url = std::str::from_utf8(&output.stdout).map_err(io)?.trim();
                    if !url.is_empty()
                        && !spec.trusted_lfs_urls.iter().any(|approved| approved == url)
                    {
                        return Err(issue(
                            IssueCode::PermissionRequired,
                            "Checkout declares an LFS endpoint outside the reviewed trusted sources",
                        ));
                    }
                } else if output.status.code() != Some(1) {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        "Checkout LFS configuration is unreadable",
                    ));
                }
            }
            let installed = git::git(Some(&parent), [OsStr::new("lfs"), OsStr::new("version")])
                .output()
                .map_err(io)?;
            if !installed.status.success() {
                return Err(issue(
                    IssueCode::UnsupportedCapability,
                    "Git LFS is required; install it before retrying materialization",
                ));
            }
            if !lfs_inventory(&parent)?.is_empty() {
                self.git_step(
                    request,
                    &parent,
                    [
                        OsStr::new("lfs"),
                        OsStr::new("install"),
                        OsStr::new("--local"),
                        OsStr::new("--skip-repo"),
                    ],
                    capture,
                )
                .await?;
                let head = checked_git(&parent, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
                self.lfs_step(
                    request,
                    &parent,
                    [
                        OsStr::new("lfs"),
                        OsStr::new("fetch"),
                        OsStr::new("origin"),
                        OsStr::new(head.trim()),
                    ],
                    capture,
                )
                .await?;
                self.lfs_step(
                    request,
                    &parent,
                    [OsStr::new("lfs"), OsStr::new("checkout")],
                    capture,
                )
                .await?;
                verify_lfs_files(&parent)?;
                self.lfs_step(
                    request,
                    &parent,
                    [OsStr::new("lfs"), OsStr::new("fsck")],
                    capture,
                )
                .await?;
            }
            git::detach_borrowed_objects(project_root, &parent)?;
            for (_, relative) in if spec.submodules {
                submodules(&parent)?
            } else {
                Vec::new()
            } {
                let path = parent.join(&relative).canonicalize().map_err(io)?;
                if !path.starts_with(project_root) {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "LFS submodule left the reviewed clone",
                    ));
                }
                pending.push(path);
            }
        }
        Ok(())
    }

    pub(super) async fn verify_ready(
        &self,
        operation: &CloneOperation,
        stage: &PhysicalBinding,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let root = stage.observed_path();
        self.verify_acquired(operation, stage)?;
        let head = checked_git(root, [OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
        if head.trim() != operation.public.review.source_commit {
            return Err(issue(
                IssueCode::Conflict,
                "Clone HEAD changed after materialization",
            ));
        }
        let spec = &operation.public.review.spec;
        if spec.submodules {
            let status = checked_git(
                root,
                [
                    OsStr::new("submodule"),
                    OsStr::new("status"),
                    OsStr::new("--recursive"),
                ],
            )?;
            if status.lines().any(|line| line.starts_with(['-', '+', 'U'])) {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "A recursive gitlink is missing or has the wrong commit",
                ));
            }
        }
        let remotes = checked_git(root, [OsStr::new("remote")])?;
        let actual: HashSet<_> = remotes.lines().collect();
        let expected: HashSet<_> = spec.remotes.iter().map(|r| r.name.as_str()).collect();
        if actual != expected {
            return Err(issue(
                IssueCode::Conflict,
                "Resulting remotes differ from the reviewed selection",
            ));
        }
        for remote in &spec.remotes {
            let url = checked_git(
                root,
                [
                    OsStr::new("remote"),
                    OsStr::new("get-url"),
                    OsStr::new(&remote.name),
                ],
            )?;
            if url.trim() != remote.url {
                return Err(issue(IssueCode::Conflict, "A resulting remote URL changed"));
            }
        }
        verify_clean_tree(root, spec)?;
        self.git_step(
            operation.public.request,
            root,
            [
                OsStr::new("fsck"),
                OsStr::new("--full"),
                OsStr::new("--no-reflogs"),
            ],
            capture,
        )
        .await?;
        if spec.lfs {
            verify_lfs_files(root)?;
        }
        if stage.volume().as_str() != operation.public.review.volume_uuid {
            return Err(issue(
                IssueCode::OfflineVolume,
                "Clone stage is on a different volume",
            ));
        }
        Ok(())
    }
}

fn verify_clean_tree(root: &Path, spec: &CloneSpec) -> Result<()> {
    let mut status_args = Vec::new();
    if spec.lfs {
        status_args.extend([
            OsString::from("-c"),
            OsString::from("filter.lfs.clean=git lfs clean -- %f"),
            OsString::from("-c"),
            OsString::from("filter.lfs.process=git lfs filter-process"),
        ]);
    }
    status_args.extend([
        OsString::from("status"),
        OsString::from("--porcelain=v1"),
        OsString::from("-z"),
        OsString::from("--untracked-files=all"),
    ]);
    let status = git::git(Some(root), status_args).output().map_err(io)?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone stage has modified or untracked data; inspect before publication",
        ));
    }
    Ok(())
}

fn checked_git<I, S>(root: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git::git(Some(root), args).output().map_err(io)?;
    if !output.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git verification failed; inspect retained operation output and stage",
        ));
    }
    if output.stdout.len() > 8 * 1024 * 1024 {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git metadata response is unexpectedly large",
        ));
    }
    String::from_utf8(output.stdout).map_err(io)
}

fn submodules(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    if !root.join(".gitmodules").exists() {
        return Ok(vec![]);
    }
    let output = git::git(
        Some(root),
        [
            OsStr::new("config"),
            OsStr::new("-z"),
            OsStr::new("--file"),
            OsStr::new(".gitmodules"),
            OsStr::new("--get-regexp"),
            OsStr::new("^submodule\\..*\\.path$"),
        ],
    )
    .output()
    .map_err(io)?;
    if !output.status.success() {
        return Err(issue(
            IssueCode::InvalidInput,
            "Submodule manifest is invalid",
        ));
    }
    let mut found = Vec::new();
    for pair in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|v| !v.is_empty())
    {
        let text = std::str::from_utf8(pair).map_err(io)?;
        let (key, value) = text
            .split_once('\n')
            .ok_or_else(|| corrupt("Invalid submodule manifest pair"))?;
        let name = key
            .strip_prefix("submodule.")
            .and_then(|v| v.strip_suffix(".path"))
            .ok_or_else(|| corrupt("Invalid submodule name"))?;
        let path = PathBuf::from(value);
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            || value.is_empty()
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Submodule path must be relative without traversal",
            ));
        }
        found.push((name.to_string(), path));
    }
    Ok(found)
}

fn verify_lfs_files(root: &Path) -> Result<()> {
    for file in lfs_inventory(root)? {
        let name = file
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| corrupt("LFS path missing"))?;
        let oid = file
            .get("oid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| corrupt("LFS object ID missing"))?;
        let path = root.join(name);
        if path.components().any(|c| matches!(c, Component::ParentDir))
            || !path.canonicalize().map_err(io)?.starts_with(root)
        {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "LFS target escaped its checkout",
            ));
        }
        let mut source = File::open(&path).map_err(io)?;
        let mut sha = Sha256::new();
        std::io::copy(&mut source, &mut sha).map_err(io)?;
        if format!("{:x}", sha.finalize()) != oid.trim_start_matches("sha256:") {
            return Err(issue(
                IssueCode::RecoveryRequired,
                format!("LFS content for {name} is missing or differs from its pointer"),
            ));
        }
    }
    Ok(())
}

fn lfs_inventory(root: &Path) -> Result<Vec<serde_json::Value>> {
    let output = git::git(
        Some(root),
        [
            OsStr::new("lfs"),
            OsStr::new("ls-files"),
            OsStr::new("--json"),
        ],
    )
    .output()
    .map_err(io)?;
    if !output.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "LFS inventory is unavailable",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(io)?;
    match value.get("files") {
        Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(files)) => Ok(files.clone()),
        _ => Err(corrupt("Invalid Git LFS files inventory")),
    }
}
