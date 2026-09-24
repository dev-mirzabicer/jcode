//! Offline history capture. Source refs are never changed. Temporary preservation
//! refs live in a private bare repository, and every bundle is restored without
//! alternates before it can serve as preservation evidence.
use super::*;
use crate::execution::owned_child::OwnedChild;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
mod lfs;

pub(super) fn verify_lfs_manifest(path: &Path) -> Result<()> {
    let objects: BTreeMap<String, lfs::LfsObject> = storage::read_json(path)?;
    for object in objects.values() {
        let metadata = std::fs::symlink_metadata(&object.preserved).map_err(io)?;
        if !metadata.is_file()
            || metadata.len() != object.size
            || backup::file_digest(&object.preserved)? != object.oid
        {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "A preserved historical LFS payload is no longer intact",
            ));
        }
    }
    Ok(())
}

pub(super) fn dependent_worktrees(stored: &StoredCloseout) -> Result<Vec<Issue>> {
    use std::os::unix::ffi::OsStrExt;
    let history = stored.history.as_ref().ok_or_else(|| {
        issue(
            IssueCode::IncompleteCapture,
            "Git history has not been inventoried",
        )
    })?;
    if Some(backup::file_digest(history)?) != stored.history_digest {
        return Err(corrupt("Git inventory changed"));
    }
    let snapshots: Vec<RepositorySnapshot> = storage::read_json(history)?;
    let mut issues = Vec::new();
    for snapshot in snapshots {
        inventory::verify_tree(&snapshot.administration)?;
        if !snapshot
            .common_directory
            .starts_with(stored.binding.observed_path())
        {
            continue;
        }
        let worktrees = snapshot.capture_directory.join("worktrees");
        if snapshot.observations.get("worktrees") != Some(&backup::file_digest(&worktrees)?) {
            return Err(corrupt("Git worktree observation changed"));
        }
        for field in std::fs::read(worktrees).map_err(io)?.split(|b| *b == 0) {
            if let Some(path) = field.strip_prefix(b"worktree ") {
                let path = Path::new(std::ffi::OsStr::from_bytes(path));
                let resolved = crate::location::native_files::resolve_target(path).map_err(io)?;
                if !resolved.starts_with(stored.binding.observed_path()) {
                    issues.push(issue(
                        IssueCode::Referenced,
                        format!(
                            "An external worktree still depends on Git metadata being removed: {}",
                            path.display()
                        ),
                    ));
                }
            }
        }
    }
    Ok(issues)
}

fn discover_repositories(root: &Path) -> Result<BTreeSet<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let device = std::fs::symlink_metadata(root).map_err(io)?.dev();
    let mut roots = BTreeSet::from([root.to_path_buf()]);
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).map_err(io)?;
        if !metadata.is_dir() || metadata.dev() != device {
            continue;
        }
        let marker = path.join(".git");
        if std::fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file() || m.is_dir()) {
            roots.insert(path.clone());
        } else if path.join("HEAD").is_file()
            && path.join("objects").is_dir()
            && path.join("refs").is_dir()
        {
            roots.insert(path);
            continue;
        }
        for entry in std::fs::read_dir(&path).map_err(io)? {
            let entry = entry.map_err(io)?;
            if entry.file_name() != ".git" && entry.file_type().map_err(io)?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(roots)
}

async fn prepare_index_view(
    service: &WorkspaceService,
    operation: OperationId,
    root: &Path,
    directory: &Path,
    capture: &dyn OutputCapture,
) -> Result<PathBuf> {
    storage::private_dir(directory)?;
    let archive = archive::Archive::open(directory)?;
    let location = directory.join("source-index");
    run(
        service,
        operation,
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        None,
        &location,
        capture,
    )
    .await?;
    let text = std::fs::read_to_string(location).map_err(io)?;
    let path = PathBuf::from(
        text.strip_suffix('\n')
            .ok_or_else(|| corrupt("Git index path missing terminator"))?,
    );
    let target = directory.join("index");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(target),
        Err(error) => return Err(io(error)),
    };
    if !metadata.is_file() {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Index observation requires a regular index file",
        ));
    }
    let witness = inventory::Witness::of(&metadata)?;
    let item = inventory::Item {
        entry: CloseoutEntry {
            id: String::new(),
            path: "index".into(),
            kind: CloseoutEntryKind::File,
            bytes: metadata.len(),
            sha256: Some(inventory::hash_file(&path, &witness)?),
            link_target: None,
            links: witness.links,
            mode: witness.mode,
            facts: Vec::new(),
            blockers: Vec::new(),
        },
        witness: Some(witness),
    };
    files::copy_entry(&archive, &item, &path, &target)?;
    // Git may freshen the source shared-index timestamp while expanding this
    // private copy. Do this before filesystem capture. Subsequent observation
    // keeps the original repository/configuration but reads only the full copy.
    let runner = GitExecution {
        service,
        operation,
        capture,
        archive: None,
        index_file: Some(&target),
    };
    runner
        .run(
            root,
            &[
                "-c",
                "core.splitIndex=false",
                "update-index",
                "--no-split-index",
            ],
            None,
            &directory.join("expand.log"),
        )
        .await?;
    Ok(target)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct RepositorySnapshot {
    pub root: PathBuf,
    pub git_directory: PathBuf,
    pub common_directory: PathBuf,
    pub object_format: String,
    pub refs: BTreeMap<String, String>,
    pub reflog: BTreeSet<String>,
    pub head: Option<String>,
    pub observations: BTreeMap<String, String>,
    pub capture_directory: PathBuf,
    pub administration: inventory::TreeSnapshot,
    pub index_blobs: BTreeSet<String>,
}

impl WorkspaceService {
    /// Complete refresh used by the runtime owner. Private Git diagnostics and
    /// filesystem observations are published together only after revalidation.
    pub async fn refresh_closeout(
        &self,
        operation: OperationId,
        expected: Revision,
        capture: &dyn OutputCapture,
    ) -> Result<CloseoutRecord> {
        self.refresh_closeout_in(
            operation,
            expected,
            &crate::storage::jcode_dir().map_err(io)?,
            capture,
        )
        .await
    }

    pub async fn refresh_closeout_in(
        &self,
        operation: OperationId,
        expected: Revision,
        session_root: &Path,
        capture: &dyn OutputCapture,
    ) -> Result<CloseoutRecord> {
        let _operation = self.closeout_lease(operation)?;
        let before = load(&self.connection()?, operation)?;
        require_current(&self.connection()?, &before, expected)?;
        // Existing instruction resolution may read Git's split index, whose
        // maintenance timestamp Git refreshes. Observe references before taking
        // immutable filesystem witnesses, never relax those witnesses afterward.
        let references = self.closeout_references(&before, session_root)?;
        self.resolver
            .resolve_directory(&before.binding)
            .map_err(io)?;
        let root = before.binding.observed_path().to_path_buf();
        let roots = discover_repositories(&root)?;
        let directory = self
            .root
            .join("closeout-inventories")
            .join(operation.to_string())
            .join(format!("git-{}", RequestId::new()));
        storage::private_dir(&directory)?;
        let mut views = BTreeMap::new();
        for (index, root) in roots.iter().enumerate() {
            let view = prepare_index_view(
                self,
                operation,
                root,
                &directory.join(format!("view-{index}")),
                capture,
            )
            .await?;
            views.insert(root.clone(), view);
        }
        let record = self.inventory_closeout(operation, expected)?;
        let mut stored = load(&self.connection()?, operation)?;
        let mut snapshots = Vec::new();
        let mut entries = Vec::new();
        for (index, root) in roots.into_iter().enumerate() {
            let snapshot = observe(
                self,
                operation,
                &root,
                &directory.join(index.to_string()),
                capture,
                &views[&root],
            )
            .await?;
            entries.push(CloseoutEntry {
                id: String::new(),
                path: root
                    .strip_prefix(stored.binding.observed_path())
                    .map_err(io)?
                    .into(),
                kind: CloseoutEntryKind::Git,
                bytes: 0,
                sha256: Some(digest(encode(&snapshot)?.as_bytes())),
                link_target: None,
                links: 0,
                mode: 0,
                facts: vec![
                    format!(
                        "{} refs; {} reflog tips; HEAD {:?}",
                        snapshot.refs.len(),
                        snapshot.reflog.len(),
                        snapshot.head
                    ),
                    format!(
                        "Git directory: {}; common directory: {}; complete observations: {}",
                        snapshot.git_directory.display(),
                        snapshot.common_directory.display(),
                        snapshot.capture_directory.display()
                    ),
                ],
                blockers: vec![],
            });
            snapshots.push(snapshot);
        }
        let references_path = directory.join("references.json");
        storage::atomic_json(&references_path, &references)?;
        stored.references = Some((
            references_path.clone(),
            backup::file_digest(&references_path)?,
        ));
        entries.push(references::entry(&references)?);
        inventory::verify_source(&stored)?;
        let history = directory.join("repositories.json");
        storage::atomic_json(&history, &snapshots)?;
        stored.history_digest = Some(backup::file_digest(&history)?);
        stored.history = Some(history);
        inventory::append(&mut stored, entries, &directory)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        require_current(
            &transaction,
            &load(&transaction, operation)?,
            record.revision,
        )?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        stored.record.revision = storage::status(&transaction)?.revision;
        save(&transaction, &stored)?;
        transaction.commit().map_err(io)?;
        self.closeout_backup(stored.record)
    }
}

pub(super) async fn observe(
    service: &WorkspaceService,
    operation: OperationId,
    root: &Path,
    destination: &Path,
    capture: &dyn OutputCapture,
    index_file: &Path,
) -> Result<RepositorySnapshot> {
    storage::private_dir(destination)?;
    let runner = GitExecution {
        service,
        operation,
        capture,
        archive: None,
        index_file: Some(index_file),
    };
    let mut observations = BTreeMap::new();
    for (name, arguments) in [
        (
            "identity",
            vec![
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
        ),
        ("format", vec!["rev-parse", "--show-object-format"]),
        (
            "refs",
            vec!["for-each-ref", "--format=%(objectname) %(refname)"],
        ),
        ("reflog", vec!["reflog", "show", "--all", "--format=%H"]),
        ("worktrees", vec!["worktree", "list", "--porcelain", "-z"]),
        ("shallow", vec!["rev-parse", "--is-shallow-repository"]),
        ("bare", vec!["rev-parse", "--is-bare-repository"]),
    ] {
        let output = destination.join(name);
        runner.run(root, &arguments, None, &output).await?;
        observations.insert(name.into(), backup::file_digest(&output)?);
    }
    let identity = std::fs::read_to_string(destination.join("identity")).map_err(io)?;
    let paths: Vec<_> = identity.lines().collect();
    if paths.len() != 2 {
        return Err(corrupt("Invalid Git metadata identity"));
    }
    let git_directory = PathBuf::from(paths[0]);
    let common_directory = PathBuf::from(paths[1]);
    let object_format = std::fs::read_to_string(destination.join("format"))
        .map_err(io)?
        .trim()
        .to_owned();
    if !matches!(object_format.as_str(), "sha1" | "sha256") {
        return Err(corrupt("Unsupported Git object format"));
    }
    if std::fs::read_to_string(destination.join("shallow"))
        .map_err(io)?
        .trim()
        != "false"
    {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Shallow history must be completed or independently preserved before checkout removal",
        ));
    }
    let mut refs = BTreeMap::new();
    for line in BufReader::new(File::open(destination.join("refs")).map_err(io)?).lines() {
        let line = line.map_err(io)?;
        let (oid, name) = line
            .split_once(' ')
            .ok_or_else(|| corrupt("Malformed Git ref observation"))?;
        validate_oid(oid)?;
        refs.insert(name.into(), oid.into());
    }
    let mut reflog = BTreeSet::new();
    for line in BufReader::new(File::open(destination.join("reflog")).map_err(io)?).lines() {
        let line = line.map_err(io)?;
        validate_oid(&line)?;
        reflog.insert(line);
    }
    // An unborn repository has no HEAD, but can still contain index/user data.
    let head = if refs.is_empty() && reflog.is_empty() && !git_directory.join("HEAD").exists() {
        None
    } else {
        let output = destination.join("head");
        match runner
            .run(root, &["rev-parse", "--verify", "HEAD"], None, &output)
            .await
        {
            Ok(()) => {
                let oid = std::fs::read_to_string(output)
                    .map_err(io)?
                    .trim()
                    .to_owned();
                validate_oid(&oid)?;
                Some(oid)
            }
            Err(error) if refs.is_empty() && reflog.is_empty() => {
                // Prove an unborn symbolic branch, not an unreadable/missing object.
                runner
                    .run(
                        root,
                        &["symbolic-ref", "HEAD"],
                        None,
                        &destination.join("unborn"),
                    )
                    .await
                    .map_err(|_| error)?;
                None
            }
            Err(error) => return Err(error),
        }
    };
    if std::fs::read_to_string(destination.join("bare"))
        .map_err(io)?
        .trim()
        == "false"
    {
        for (name, arguments) in [
            (
                "status",
                vec![
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--ignored=matching",
                    "--ignore-submodules=all",
                ],
            ),
            ("index", vec!["ls-files", "--stage", "-z"]),
            ("submodules", vec!["submodule", "status"]),
        ] {
            let output = destination.join(name);
            runner.run(root, &arguments, None, &output).await?;
            observations.insert(name.into(), backup::file_digest(&output)?);
        }
    }
    let mut index_blobs = BTreeSet::new();
    if destination.join("index").is_file() {
        for entry in BufReader::new(File::open(destination.join("index")).map_err(io)?).split(0) {
            let entry = entry.map_err(io)?;
            if entry.is_empty() {
                continue;
            }
            let end = entry
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| corrupt("Invalid Git index entry"))?;
            let header = std::str::from_utf8(&entry[..end])
                .map_err(corrupt)?
                .split(' ')
                .collect::<Vec<_>>();
            if header.len() != 3 || !matches!(header[2], "0" | "1" | "2" | "3") {
                return Err(corrupt("Invalid Git index stage"));
            }
            validate_oid(header[1])?;
            if header[0] != "160000" && !header[1].bytes().all(|byte| byte == b'0') {
                index_blobs.insert(header[1].into());
            }
        }
    }
    let administration = inventory::capture_git_administration(
        &git_directory,
        &destination.join("administration.jsonl"),
    )?;
    Ok(RepositorySnapshot {
        root: root.into(),
        git_directory,
        common_directory,
        object_format,
        refs,
        reflog,
        head,
        observations,
        capture_directory: destination.into(),
        administration,
        index_blobs,
    })
}

pub(super) async fn preserve(
    service: &WorkspaceService,
    operation: OperationId,
    snapshot: &RepositorySnapshot,
    directory: &Path,
    capture: &dyn OutputCapture,
    archive: &archive::Archive,
) -> Result<PathBuf> {
    archive.directory(directory)?;
    let runner = GitExecution {
        service,
        operation,
        capture,
        archive: Some(archive),
        index_file: None,
    };
    inventory::verify_tree(&snapshot.administration)?;
    let stage = directory.join(format!("history-{}", RequestId::new()));
    archive.directory(&stage)?;
    let bare = stage.join("source.git");
    let args = vec![
        "init".into(),
        "--bare".into(),
        "--template=".into(),
        format!("--object-format={}", snapshot.object_format),
        bare.to_string_lossy().into_owned(),
    ];
    runner
        .run_owned(&stage, &args, None, &stage.join("init.log"))
        .await?;
    let objects = snapshot
        .common_directory
        .join("objects")
        .canonicalize()
        .map_err(io)?;
    if objects.to_string_lossy().contains(['\n', '\r']) {
        return Err(issue(
            IssueCode::InvalidInput,
            "Git object path cannot be represented as a temporary alternate",
        ));
    }
    // Only the temporary source borrows objects. Neither source refs nor the
    // restored evidence repository acquires any new dependency or mutation.
    let mut alternates = archive.file(&bare.join("objects/info/alternates"))?;
    writeln!(alternates, "{}", objects.display()).map_err(io)?;
    alternates.sync_all().map_err(io)?;
    let mut refs = snapshot.refs.clone();
    let prefix = format!("refs/jcode-closeout/{operation}");
    if refs.keys().any(|name| name.starts_with(&prefix)) {
        return Err(issue(
            IssueCode::Conflict,
            "Preservation ref namespace already exists in source",
        ));
    }
    if let Some(head) = &snapshot.head {
        refs.insert(format!("{prefix}/head"), head.clone());
    }
    if !snapshot.index_blobs.is_empty() {
        let input_path = stage.join("index-tree.input");
        let mut input = archive.file(&input_path)?;
        for oid in &snapshot.index_blobs {
            writeln!(input, "100644 blob {oid}\t{oid}").map_err(io)?;
        }
        input.sync_all().map_err(io)?;
        drop(input);
        let tree_path = stage.join("index-tree.oid");
        runner
            .run(&bare, &["mktree"], Some(&input_path), &tree_path)
            .await?;
        let tree = std::fs::read_to_string(&tree_path).map_err(io)?;
        let commit_path = stage.join("index-commit.oid");
        runner
            .run(
                &bare,
                &[
                    "-c",
                    "user.name=Jcode preservation",
                    "-c",
                    "user.email=preservation@localhost",
                    "-c",
                    "commit.gpgSign=false",
                    "commit-tree",
                    tree.trim(),
                    "-m",
                    "Preserved index objects",
                ],
                None,
                &commit_path,
            )
            .await?;
        let commit = std::fs::read_to_string(commit_path).map_err(io)?;
        validate_oid(commit.trim())?;
        refs.insert(format!("{prefix}/index"), commit.trim().into());
    }
    for name in snapshot.refs.keys() {
        if let Some(original) = name.strip_prefix("refs/replace/") {
            validate_oid(original)?;
            refs.insert(format!("{prefix}/replaced/{original}"), original.into());
        }
    }
    for oid in &snapshot.reflog {
        refs.insert(format!("{prefix}/reflog/{oid}"), oid.clone());
    }
    let commands = stage.join("refs.input");
    let mut input = archive.file(&commands)?;
    for (name, oid) in &refs {
        validate_oid(oid)?;
        if !name.starts_with("refs/") || name.chars().any(char::is_whitespace) {
            return Err(corrupt("Invalid preserved ref name"));
        }
        writeln!(input, "create {name} {oid}").map_err(io)?;
    }
    input.sync_all().map_err(io)?;
    drop(input);
    runner
        .run(
            &bare,
            &["update-ref", "--stdin"],
            Some(&commands),
            &stage.join("refs.log"),
        )
        .await?;
    let admin_directory = stage.join("administration");
    archive.directory(&admin_directory)?;
    let admin_manifest = files::preserve_tree(
        &snapshot.administration,
        &BTreeMap::new(),
        true,
        &admin_directory,
        archive,
    )?;
    archive.json(
        &stage.join("administration.json"),
        &(
            admin_manifest.clone(),
            backup::file_digest(&admin_manifest)?,
        ),
    )?;
    let bundle = stage.join(if refs.is_empty() {
        "empty-history.json"
    } else {
        "history.bundle"
    });
    if refs.is_empty() {
        archive.json(&bundle, &refs)?;
    } else {
        runner
            .run_owned(
                &bare,
                &[
                    "bundle".into(),
                    "create".into(),
                    bundle.to_string_lossy().into_owned(),
                    "--all".into(),
                ],
                None,
                &stage.join("bundle.log"),
            )
            .await?;
        File::open(&bundle)
            .and_then(|file| file.sync_all())
            .map_err(io)?;
    }
    let restored = stage.join("restored.git");
    runner
        .run_owned(
            &stage,
            &[
                "init".into(),
                "--bare".into(),
                "--template=".into(),
                format!("--object-format={}", snapshot.object_format),
                restored.to_string_lossy().into_owned(),
            ],
            None,
            &stage.join("restore-init.log"),
        )
        .await?;
    if !refs.is_empty() {
        runner
            .run_owned(
                &restored,
                &[
                    "-c".into(),
                    "protocol.file.allow=always".into(),
                    "fetch".into(),
                    "--no-write-fetch-head".into(),
                    bundle.to_string_lossy().into_owned(),
                    "refs/*:refs/*".into(),
                ],
                None,
                &stage.join("restore.log"),
            )
            .await?;
    }
    inventory::visit_tree(&snapshot.administration, |item| {
        if item.entry.path.components().count() == 1
            && item.entry.path.file_name().is_some_and(|name| {
                name == "index" || name.to_string_lossy().starts_with("sharedindex.")
            })
        {
            if item.entry.kind != CloseoutEntryKind::File {
                return Err(issue(
                    IssueCode::PreservationIncomplete,
                    "Git index metadata must be a regular file for independent restoration",
                ));
            }
            files::copy_entry(
                archive,
                &item,
                &admin_directory.join("files").join(&item.entry.path),
                &restored.join(&item.entry.path),
            )?;
        }
        Ok(())
    })?;
    if let Some(expected) = snapshot.observations.get("index") {
        let index_check = stage.join("restored-index");
        runner
            .run(
                &restored,
                &["ls-files", "--stage", "-z"],
                None,
                &index_check,
            )
            .await?;
        if backup::file_digest(&index_check)? != *expected {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Restored index differs from the complete staged/conflict inventory",
            ));
        }
    }
    runner
        .run(
            &restored,
            &["fsck", "--full", "--no-reflogs"],
            None,
            &stage.join("fsck.log"),
        )
        .await?;
    let restored_refs = stage.join("restored-refs");
    runner
        .run(
            &restored,
            &["for-each-ref", "--format=%(objectname) %(refname)"],
            None,
            &restored_refs,
        )
        .await?;
    let actual = std::fs::read_to_string(restored_refs).map_err(io)?;
    let expected: String = refs
        .iter()
        .map(|(name, oid)| format!("{oid} {name}\n"))
        .collect();
    if actual != expected || restored.join("objects/info/alternates").exists() {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Restored bundle refs or object independence differ",
        ));
    }
    archive.json(&stage.join("verified-refs.json"), &refs)?;
    if !refs.is_empty() {
        lfs::preserve(&runner, snapshot, &stage, &restored, archive).await?;
    }
    inventory::verify_tree(&snapshot.administration)?;
    storage::sync_dir(&stage)?;
    Ok(bundle)
}

fn validate_oid(oid: &str) -> Result<()> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(corrupt("Invalid Git object identity"));
    }
    Ok(())
}

pub(super) async fn run(
    service: &WorkspaceService,
    operation: OperationId,
    cwd: &Path,
    args: &[&str],
    input: Option<&Path>,
    output: &Path,
    capture: &dyn OutputCapture,
) -> Result<()> {
    GitExecution {
        service,
        operation,
        capture,
        archive: None,
        index_file: None,
    }
    .run(cwd, args, input, output)
    .await
}

struct GitExecution<'a> {
    service: &'a WorkspaceService,
    operation: OperationId,
    capture: &'a dyn OutputCapture,
    archive: Option<&'a archive::Archive>,
    index_file: Option<&'a Path>,
}
impl GitExecution<'_> {
    fn command<I, S>(&self, cwd: &Path, args: I) -> Result<tokio::process::Command>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let arguments = args
            .into_iter()
            .map(|arg| {
                let path = Path::new(arg.as_ref());
                if self.archive.is_some() && path.is_absolute() {
                    relative_argument(cwd, path).into_os_string()
                } else {
                    arg.as_ref().to_owned()
                }
            })
            .collect::<Vec<_>>();
        let mut command = tokio::process::Command::from(checkout::git::git(Some(cwd), arguments));
        if let Some(index) = self.index_file {
            command.env("GIT_INDEX_FILE", index);
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let directory = match self.archive {
                Some(archive) if archive.contains(cwd) => archive.existing_directory(cwd)?,
                _ => crate::location::native_files::VerifiedDirectory::open(cwd.into())
                    .map_err(io)?,
            };
            let directory_file = directory
                .preservation_handle()
                .map_err(io)?
                .try_clone()
                .map_err(io)?;
            // SAFETY: pre_exec performs only async-signal-safe fchdir on an
            // owned descriptor. Relative Git writes stay on the selected volume.
            unsafe {
                command.pre_exec(move || {
                    if libc::fchdir(directory_file.as_raw_fd()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        Ok(command)
    }
    async fn run_owned(
        &self,
        cwd: &Path,
        args: &[String],
        input: Option<&Path>,
        output: &Path,
    ) -> Result<()> {
        self.run(
            cwd,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            input,
            output,
        )
        .await
    }

    async fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        input: Option<&Path>,
        output: &Path,
    ) -> Result<()> {
        // Status can invoke repository-local clean/process filters. Discover only
        // their keys with an owned, non-filtering config read, then disable every
        // configured filter for this observation. Never execute project setup.
        let keys = output.with_extension(format!("filters-{}", RequestId::new()));
        let query = self.command(
            cwd,
            [
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                "^filter\\..*\\.(clean|smudge|process|required)$",
            ],
        )?;
        let status = self.run_command(query, None, &keys).await?;
        if !matches!(status.code(), Some(0 | 1)) {
            return Err(issue(
                IssueCode::IncompleteCapture,
                "Cannot inspect repository filter configuration safely",
            ));
        }
        let mut arguments = Vec::new();
        for key in std::fs::read(&keys)
            .map_err(io)?
            .split(|b| *b == 0)
            .filter(|key| !key.is_empty())
        {
            let key = std::str::from_utf8(key).map_err(corrupt)?;
            if key.contains('=') || key.chars().any(char::is_control) {
                return Err(issue(
                    IssueCode::IncompleteCapture,
                    "Repository filter key cannot be safely overridden",
                ));
            }
            arguments.push("-c".to_owned());
            arguments.push(format!(
                "{key}={}",
                if key.ends_with(".required") {
                    "false"
                } else {
                    ""
                }
            ));
        }
        arguments.extend(args.iter().map(|s| s.to_string()));
        let command = self.command(cwd, arguments)?;
        if !self.run_command(command, input, output).await?.success() {
            return Err(issue(
                IssueCode::PreservationIncomplete,
                "Offline Git operation failed; inspect retained execution diagnostics",
            ));
        }
        Ok(())
    }

    async fn run_command(
        &self,
        mut command: tokio::process::Command,
        input: Option<&Path>,
        output: &Path,
    ) -> Result<std::process::ExitStatus> {
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", "/dev/null");
        // All operations are local. The only file transport exception is the exact
        // owned bundle used for the restoration above, never an arbitrary source.
        command.env("GIT_ALLOW_PROTOCOL", "file");
        if let Some(input) = input {
            command.stdin(Stdio::from(File::open(input).map_err(io)?));
        }
        let destination = match self.archive {
            Some(archive) => archive.file(output)?,
            None => storage::private_file(output, true)?,
        };
        let capture = self.capture;
        let ticket = capture.begin_process().map_err(io)?;
        let mut child = match OwnedChild::spawn(&mut command) {
            Ok(child) => child,
            Err(error) => {
                capture.finish_process(&ticket).map_err(io)?;
                return Err(io(error));
            }
        };
        if let Err(error) = capture.register_process(
            &ticket,
            child
                .id()
                .ok_or_else(|| corrupt("Git process has no PID"))?,
        ) {
            child.stop().await.map_err(io)?;
            capture.finish_process(&ticket).map_err(io)?;
            return Err(io(error));
        }
        let mut stdout = child
            .stdout()
            .ok_or_else(|| corrupt("Git stdout missing"))?;
        let mut stderr = child
            .stderr()
            .ok_or_else(|| corrupt("Git stderr missing"))?;
        let wait = async {
            loop {
                tokio::select! {
                    status = child.wait() => return status.map_err(io),
                    () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        if self.service.inspect_closeout(self.operation)?.stage == CloseoutStage::Revoked {
                            child.stop().await.map_err(io)?;
                            return Err(issue(IssueCode::PermissionRequired, "Closeout authorization was revoked"));
                        }
                    }
                }
            }
        };
        let out = drain(
            &mut stdout,
            capture,
            OutputStream::Stdout,
            Some(destination),
        );
        let err = drain(&mut stderr, capture, OutputStream::Stderr, None);
        let (status, out, err) = tokio::join!(wait, out, err);
        capture.finish_process(&ticket).map_err(io)?;
        out?;
        err?;
        if let Some(archive) = self.archive {
            archive.verify()?;
        }
        status
    }
}

fn relative_argument(cwd: &Path, target: &Path) -> PathBuf {
    let from = cwd.components().collect::<Vec<_>>();
    let to = target.components().collect::<Vec<_>>();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in common..from.len() {
        path.push("..");
    }
    for component in &to[common..] {
        path.push(component.as_os_str());
    }
    if path.as_os_str().is_empty() {
        path.push(".");
    }
    path
}

pub(super) async fn drain(
    reader: &mut (impl AsyncRead + Unpin),
    capture: &dyn OutputCapture,
    stream: OutputStream,
    file: Option<File>,
) -> Result<()> {
    let mut file = file.map(tokio::fs::File::from_std);
    let mut buffer = vec![0u8; 65536];
    let mut failure = None;
    loop {
        let count = reader.read(&mut buffer).await.map_err(io)?;
        if count == 0 {
            break;
        }
        // Even if retention fails, drain the pipe so the owned child can reach
        // quiescence. No capture failure is converted into successful evidence.
        if failure.is_none() {
            if let Err(error) = capture.write(stream, &buffer[..count]) {
                failure = Some(io(error));
            }
            if let Some(file) = &mut file
                && let Err(error) = file.write_all(&buffer[..count]).await
            {
                failure = Some(io(error));
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if let Some(file) = file {
        file.sync_all().await.map_err(io)?;
    }
    Ok(())
}
