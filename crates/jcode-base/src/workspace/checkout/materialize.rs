use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

impl WorkspaceService {
    pub(super) async fn materialize_clone_content(
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
            self.materialize_submodules(request, root, root, capture, &mut visited)
                .await?;
        }
        if spec.lfs {
            self.materialize_lfs_tree(request, root, root, spec, capture)
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

    pub(super) async fn git_step<I, S>(
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
        git::run(
            self,
            request,
            git::authorized(Some(root), args, true)?,
            capture,
        )
        .await
    }

    async fn materialize_submodules(
        &self,
        request: RequestId,
        project_root: &Path,
        root: &Path,
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
            let sources = submodule_reads(project_root, &parent)?;
            self.record_clone_sources(
                request,
                &sources
                    .iter()
                    .map(|source| source.trust.clone())
                    .collect::<Vec<_>>(),
            )?;
            for source in sources {
                let relative = source.trust.path;
                let url = source.effective;
                let needs_transport = submodule_needs_transport(&parent, &relative)?;
                let _source_use = if needs_transport {
                    self.acquire_transport_source(&source.cwd, &url)?
                } else {
                    None
                };
                self.checkpoint("clone_submodule_source_admitted")?;
                self.git_step(
                    request,
                    &parent,
                    [
                        OsStr::new("--literal-pathspecs"),
                        OsStr::new("submodule"),
                        OsStr::new("init"),
                        OsStr::new("--"),
                        relative.as_os_str(),
                    ],
                    capture,
                )
                .await?;
                let candidate = parent.join(&relative);
                if !candidate.starts_with(project_root) {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Submodule path escaped the selected clone",
                    ));
                }
                let local = url.starts_with("../")
                    || url.starts_with("./")
                    || url.starts_with("file://")
                    || url.starts_with('/');
                trust::validate_submodule_url(&url)?;
                let mut command = git::authorized(
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
                    ]
                    .into_iter()
                    .chain((!needs_transport).then_some(OsStr::new("--no-fetch")))
                    .chain([OsStr::new("--"), relative.as_os_str()]),
                    false,
                )?;
                git::command_config(
                    &mut command,
                    &format!("submodule.{}.url", source.name),
                    &url,
                )?;
                command
                    .env("GIT_PROTOCOL_FROM_USER", "0")
                    .env("GIT_LITERAL_PATHSPECS", "1");

                git::run(self, request, command, capture).await?;
                let actual = candidate.canonicalize().map_err(io)?;
                if !actual.starts_with(&parent) || !actual.starts_with(project_root) {
                    return Err(issue(
                        IssueCode::ReplacedRoot,
                        "Submodule resolved outside the checked-out repository",
                    ));
                }
                git::detach_borrowed_objects(project_root, &actual)?;
                self.checkpoint("clone_submodule_materialized")?;
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
            let installed = git::git(Some(&parent), [OsStr::new("lfs"), OsStr::new("version")])
                .output()
                .map_err(io)?;
            if !installed.status.success() {
                return Err(issue(
                    IssueCode::UnsupportedCapability,
                    "Git LFS is required; install it before retrying materialization",
                ));
            }
            if let Some(source) = lfs_source(project_root, &parent)? {
                self.record_clone_sources(request, &[source])?;
            }
            if !lfs_inventory(&parent)?.is_empty() {
                let endpoint = lfs_endpoint(&parent)?;
                let cached = lfs_cache_complete(project_root, &parent)?;
                let _source_use = if cached {
                    Vec::new()
                } else {
                    self.acquire_lfs_source(&parent, &endpoint)?
                };
                self.checkpoint("clone_lfs_source_admitted")?;
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
                if !cached {
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
                }
                self.checkpoint("clone_lfs_fetched")?;
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
        if !operation.public.pending_trust.is_empty()
            || operation
                .public
                .discovered_sources
                .iter()
                .any(|source| !trust::approved(operation, source))
        {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Clone cannot be Ready while discovered Git or LFS sources are unapproved",
            ));
        }
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
        remotes::verify_result_refs(operation, root)?;
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

fn submodule_needs_transport(parent: &Path, relative: &Path) -> Result<bool> {
    let child = parent.join(relative);
    match std::fs::symlink_metadata(child.join(".git")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(io(error)),
        Ok(_) => {}
    }
    let entry = checked_git(
        parent,
        [
            OsStr::new("--literal-pathspecs"),
            OsStr::new("ls-files"),
            OsStr::new("--stage"),
            OsStr::new("-z"),
            OsStr::new("--"),
            relative.as_os_str(),
        ],
    )?;
    let header = entry
        .split_once('\t')
        .ok_or_else(|| corrupt("Submodule index entry missing"))?
        .0
        .split_whitespace()
        .collect::<Vec<_>>();
    if header.len() != 3 || header[0] != "160000" || header[2] != "0" {
        return Err(corrupt("Submodule index entry is not one resolved gitlink"));
    }
    let oid = format!("{}^{{commit}}", header[1]);
    let output = git::git(Some(&child), ["cat-file", "-e", &oid])
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .map_err(io)?;
    Ok(!output.status.success())
}

fn lfs_cache_complete(stage: &Path, repository: &Path) -> Result<bool> {
    let media =
        crate::location::native_files::resolve_target(&local_lfs_media_directory(repository)?)
            .map_err(io)?;
    if !media.starts_with(stage) {
        return Err(issue(
            IssueCode::ReplacedRoot,
            "Clone LFS storage left the owned stage",
        ));
    }
    for file in lfs_inventory(repository)? {
        let oid = file
            .get("oid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| corrupt("LFS object ID missing"))?
            .trim_start_matches("sha256:");
        if oid.len() != 64 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(corrupt("Invalid LFS object ID"));
        }
        let path = media.join(&oid[..2]).join(&oid[2..4]).join(oid);
        let mut source = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io(error)),
        };
        let mut hash = Sha256::new();
        std::io::copy(&mut source, &mut hash).map_err(io)?;
        if format!("{:x}", hash.finalize()) != oid {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn verify_clean_tree(root: &Path, spec: &CloneSpec) -> Result<()> {
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

pub(super) fn checked_git<I, S>(root: &Path, args: I) -> Result<String>
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
    let manifest = root.join(".gitmodules");
    match std::fs::symlink_metadata(&manifest) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(issue(
                IssueCode::InvalidInput,
                "Submodule manifest is not a regular file",
            ));
        }
        Err(error) => return Err(io(error)),
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

/// Discover the exact URLs Git would read from this checked-out manifest.
/// These facts are retained before contacting a submodule transport.
pub(super) fn submodule_sources(stage: &Path, repository: &Path) -> Result<Vec<CloneTrustSource>> {
    Ok(submodule_reads(stage, repository)?
        .into_iter()
        .map(|source| source.trust)
        .collect())
}

struct SubmoduleRead {
    trust: CloneTrustSource,
    name: String,
    effective: String,
    cwd: PathBuf,
}

fn submodule_reads(stage: &Path, repository: &Path) -> Result<Vec<SubmoduleRead>> {
    let relative = repository.strip_prefix(stage).map_err(io)?.to_path_buf();
    let mut sources = Vec::new();
    for (name, path) in submodules(repository)? {
        let key = format!("submodule.{name}.url");
        let declared = config_value(repository, &["--file", ".gitmodules"], &key)?
            .ok_or_else(|| issue(IssueCode::InvalidInput, "Submodule has no transport URL"))?;
        trust::validate_submodule_url(&declared)?;
        let expected = if declared.starts_with("../") || declared.starts_with("./") {
            resolve_relative_submodule(repository, &name, &path)?
        } else {
            declared.clone()
        };
        let mut effective =
            config_value(repository, &["--local"], &key)?.unwrap_or_else(|| expected.clone());
        let child = repository.join(&path);
        let mut cwd = repository.to_path_buf();
        // An initialized submodule fetches from its own origin, not the
        // parent's URL setting. Observe that actual reader before admission.
        match std::fs::symlink_metadata(child.join(".git")) {
            Ok(_) => {
                effective = checked_git(&child, ["remote", "get-url", "origin"])?
                    .strip_suffix('\n')
                    .ok_or_else(|| corrupt("Submodule origin lacks terminator"))?
                    .to_owned();
                cwd = child;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
        trust::validate_submodule_url(&effective)?;
        // Preserve prior review identity for the ordinary Git-derived URL.
        // A local override is a different discovered source, not implicit trust.
        let url = if effective == expected {
            declared
        } else {
            effective.clone()
        };
        sources.push(SubmoduleRead {
            trust: CloneTrustSource {
                kind: CloneTrustKind::Submodule,
                repository: relative.clone(),
                path,
                url,
            },
            name,
            effective,
            cwd,
        });
    }
    sources.sort_by(|left, right| left.trust.cmp(&right.trust));
    Ok(sources)
}

fn config_value(root: &Path, source: &[&str], key: &str) -> Result<Option<String>> {
    let mut args = vec!["config", "-z"];
    args.extend_from_slice(source);
    args.extend(["--get", key]);
    let output = git::git(Some(root), args).output().map_err(io)?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() || output.stdout.last() != Some(&0) {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Cannot inspect effective submodule source",
        ));
    }
    String::from_utf8(output.stdout[..output.stdout.len() - 1].to_vec())
        .map(Some)
        .map_err(io)
}

fn resolve_relative_submodule(repository: &Path, name: &str, path: &Path) -> Result<String> {
    // Ask Git to resolve its own relative-URL policy in a private empty index.
    // Never initialize or rewrite the caller's config during trust inspection.
    let branch = git::git(
        Some(repository),
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .output()
    .map_err(io)?;
    let remote = if branch.status.success() {
        let branch = std::str::from_utf8(&branch.stdout)
            .map_err(io)?
            .strip_suffix('\n')
            .ok_or_else(|| corrupt("Git branch lacks terminator"))?;
        config_value(repository, &["--local"], &format!("branch.{branch}.remote"))?
            .unwrap_or_else(|| "origin".into())
    } else if branch.status.code() == Some(1) {
        "origin".into()
    } else {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Cannot inspect Git default remote",
        ));
    };
    if remote != "origin" {
        return Err(issue(
            IssueCode::Conflict,
            "Acquired stage changed its default remote; relative source trust remains bound to the reviewed acquisition origin",
        ));
    }
    let origin = checked_git(repository, ["remote", "get-url", &remote])?
        .strip_suffix('\n')
        .ok_or_else(|| corrupt("Git remote lacks terminator"))?
        .to_owned();
    let scratch = tempfile::tempdir().map_err(io)?;
    checked_git(scratch.path(), ["init", "--quiet"])?;
    checked_git(scratch.path(), ["remote", "add", "origin", &origin])?;
    std::fs::copy(
        repository.join(".gitmodules"),
        scratch.path().join(".gitmodules"),
    )
    .map_err(io)?;
    // Init needs only a gitlink in the index. The sentinel is never fetched,
    // committed or used as content; actual gitlinks remain verified by Git.
    let sentinel = "a".repeat(40);
    checked_git(
        scratch.path(),
        [
            OsStr::new("update-index"),
            OsStr::new("--add"),
            OsStr::new("--cacheinfo"),
            OsStr::new("160000"),
            OsStr::new(&sentinel),
            path.as_os_str(),
        ],
    )?;
    checked_git(
        scratch.path(),
        [
            OsStr::new("submodule"),
            OsStr::new("init"),
            OsStr::new("--"),
            path.as_os_str(),
        ],
    )?;
    config_value(
        scratch.path(),
        &["--local"],
        &format!("submodule.{name}.url"),
    )?
    .ok_or_else(|| corrupt("Git did not resolve the relative submodule URL"))
}

pub(super) fn lfs_source(stage: &Path, repository: &Path) -> Result<Option<CloneTrustSource>> {
    let config = repository.join(".lfsconfig");
    match std::fs::symlink_metadata(&config) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Git LFS falls back to the index/HEAD when the working file is
            // absent. Such a stage is not an approved empty configuration.
            let tracked = git::git(
                Some(repository),
                ["ls-files", "--error-unmatch", ".lfsconfig"],
            )
            .output()
            .map_err(io)?;
            if tracked.status.success() {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Tracked LFS configuration is missing from the stage; inspect before transfer",
                ));
            }
        }
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(issue(
                IssueCode::InvalidInput,
                "LFS configuration is not a regular file",
            ));
        }
        Err(error) => return Err(io(error)),
    }
    let origin = checked_git(repository, ["remote", "get-url", "origin"])?;
    let origin = origin.trim();
    if origin.is_empty() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Clone acquisition origin is missing",
        ));
    }
    // Query the same Git LFS implementation and command environment that will
    // fetch. This covers lfs.url, remote.origin.lfsurl, their precedence and
    // local Git configuration, without trying to duplicate Git LFS policy.
    let effective = lfs_endpoint(repository)?;
    let baseline = tempfile::tempdir().map_err(io)?;
    let init = git::git(
        None,
        [
            OsStr::new("init"),
            OsStr::new("--bare"),
            OsStr::new("--quiet"),
            baseline.path().as_os_str(),
        ],
    )
    .output()
    .map_err(io)?;
    if !init.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Could not inspect the clean Git LFS source endpoint",
        ));
    }
    let add = git::git(
        Some(baseline.path()),
        [
            OsStr::new("remote"),
            OsStr::new("add"),
            OsStr::new("origin"),
            OsStr::new(origin),
        ],
    )
    .output()
    .map_err(io)?;
    if !add.status.success() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Could not inspect the reviewed Git LFS source endpoint",
        ));
    }
    if effective == lfs_endpoint(baseline.path())? {
        return Ok(None);
    }
    checked_git_url(&effective)?;
    let local = ["lfs.url", "remote.origin.lfsurl"]
        .into_iter()
        .map(|key| local_lfs_value(repository, key))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .any(|value| value == effective);
    Ok(Some(CloneTrustSource {
        kind: CloneTrustKind::Lfs,
        repository: repository.strip_prefix(stage).map_err(io)?.to_path_buf(),
        path: if local || !config.exists() {
            PathBuf::from(".git/config")
        } else {
            PathBuf::from(".lfsconfig")
        },
        url: effective,
    }))
}

fn local_lfs_value(repository: &Path, key: &str) -> Result<Option<String>> {
    let output = git::git(Some(repository), ["config", "--local", "--get", key])
        .output()
        .map_err(io)?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() || output.stdout.len() > 8192 {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Repository-local Git LFS endpoint configuration is invalid",
        ));
    }
    Ok(Some(
        String::from_utf8(output.stdout)
            .map_err(io)?
            .trim_end()
            .to_owned(),
    ))
}

fn lfs_environment(repository: &Path) -> Result<String> {
    let output = git::git_lfs(Some(repository), ["lfs", "env"])
        .output()
        .map_err(io)?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git LFS cannot report its effective endpoint before transfer",
        ));
    }
    String::from_utf8(output.stdout).map_err(io)
}

pub(super) fn local_lfs_media_directory(repository: &Path) -> Result<PathBuf> {
    let text = lfs_environment(repository)?;
    let mut media = text
        .lines()
        .filter_map(|line| line.strip_prefix("LocalMediaDir="));
    let directory = media
        .next()
        .ok_or_else(|| corrupt("Git LFS did not identify local media storage"))?;
    if media.next().is_some() || !Path::new(directory).is_absolute() {
        return Err(corrupt("Git LFS media storage is ambiguous"));
    }
    Ok(directory.into())
}

fn lfs_endpoint(repository: &Path) -> Result<String> {
    let text = lfs_environment(repository)?;
    let mut endpoints = text
        .lines()
        .filter_map(|line| line.strip_prefix("Endpoint="));
    let value = endpoints.next().ok_or_else(|| {
        issue(
            IssueCode::RecoveryRequired,
            "Git LFS reported no effective transfer endpoint",
        )
    })?;
    if endpoints.next().is_some() {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git LFS reported ambiguous transfer endpoints",
        ));
    }
    let (endpoint, auth) = value.rsplit_once(" (auth=").ok_or_else(|| {
        issue(
            IssueCode::RecoveryRequired,
            "Git LFS endpoint format changed; no transfer was started",
        )
    })?;
    if endpoint.is_empty() || !auth.ends_with(')') {
        return Err(issue(
            IssueCode::RecoveryRequired,
            "Git LFS endpoint format changed; no transfer was started",
        ));
    }
    Ok(endpoint.to_owned())
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
