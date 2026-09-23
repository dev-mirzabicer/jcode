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
        let _operation = self.closeout_lease(operation)?;
        let record = self.inventory_closeout(operation, expected)?;
        let mut stored = { load(&self.connection()?, operation)? };
        let root = stored.binding.observed_path().to_path_buf();
        let mut roots = BTreeSet::from([root.clone()]);
        inventory::visit(&stored, |item| {
            if item
                .entry
                .path
                .file_name()
                .is_some_and(|name| name == ".git")
                && matches!(
                    item.entry.kind,
                    CloseoutEntryKind::File | CloseoutEntryKind::Directory
                )
                && let Some(parent) = item.entry.path.parent()
            {
                roots.insert(root.join(parent));
            }
            // Nested bare repositories have no .git marker.
            if item.entry.kind == CloseoutEntryKind::Directory {
                let path = root.join(&item.entry.path);
                if path.join("HEAD").is_file()
                    && path.join("objects").is_dir()
                    && path.join("refs").is_dir()
                    && !item
                        .entry
                        .path
                        .components()
                        .any(|c| c.as_os_str() == ".git")
                {
                    roots.insert(path);
                }
            }
            Ok(())
        })?;
        let directory = self
            .root
            .join("closeout-inventories")
            .join(operation.to_string())
            .join(format!("git-{}", RequestId::new()));
        storage::private_dir(&directory)?;
        let mut snapshots = Vec::new();
        let mut entries = Vec::new();
        for (index, root) in roots.into_iter().enumerate() {
            let snapshot = observe(
                self,
                operation,
                &root,
                &directory.join(index.to_string()),
                capture,
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
) -> Result<RepositorySnapshot> {
    storage::private_dir(destination)?;
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
        run(service, operation, root, &arguments, None, &output, capture).await?;
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
        match run(
            service,
            operation,
            root,
            &["rev-parse", "--verify", "HEAD"],
            None,
            &output,
            capture,
        )
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
                run(
                    service,
                    operation,
                    root,
                    &["symbolic-ref", "HEAD"],
                    None,
                    &destination.join("unborn"),
                    capture,
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
                ],
            ),
            ("index", vec!["ls-files", "--stage", "-z"]),
            ("submodules", vec!["submodule", "status", "--recursive"]),
        ] {
            let output = destination.join(name);
            run(service, operation, root, &arguments, None, &output, capture).await?;
            observations.insert(name.into(), backup::file_digest(&output)?);
        }
    }
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
    })
}

pub(super) async fn preserve(
    service: &WorkspaceService,
    operation: OperationId,
    snapshot: &RepositorySnapshot,
    directory: &Path,
    capture: &dyn OutputCapture,
) -> Result<PathBuf> {
    storage::private_dir(directory)?;
    let stage = directory.join(format!("history-{}", RequestId::new()));
    storage::private_dir(&stage)?;
    let bare = stage.join("source.git");
    let args = vec![
        "init".into(),
        "--bare".into(),
        "--template=".into(),
        format!("--object-format={}", snapshot.object_format),
        bare.to_string_lossy().into_owned(),
    ];
    run_owned(
        service,
        operation,
        &stage,
        &args,
        None,
        &stage.join("init.log"),
        capture,
    )
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
    std::fs::write(
        bare.join("objects/info/alternates"),
        format!("{}\n", objects.display()),
    )
    .map_err(io)?;
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
    for oid in &snapshot.reflog {
        refs.insert(format!("{prefix}/reflog/{oid}"), oid.clone());
    }
    let commands = stage.join("refs.input");
    let mut input = storage::private_file(&commands, true)?;
    for (name, oid) in &refs {
        validate_oid(oid)?;
        if !name.starts_with("refs/") || name.chars().any(char::is_whitespace) {
            return Err(corrupt("Invalid preserved ref name"));
        }
        writeln!(input, "create {name} {oid}").map_err(io)?;
    }
    input.sync_all().map_err(io)?;
    drop(input);
    run(
        service,
        operation,
        &bare,
        &["update-ref", "--stdin"],
        Some(&commands),
        &stage.join("refs.log"),
        capture,
    )
    .await?;
    if refs.is_empty() {
        // Empty history has no bundle. The explicit reference manifest and
        // separately preserved index/worktree still establish what existed.
        storage::atomic_json(&stage.join("empty-history.json"), &refs)?;
        return Ok(stage.join("empty-history.json"));
    }
    let bundle = stage.join("history.bundle");
    run_owned(
        service,
        operation,
        &bare,
        &[
            "bundle".into(),
            "create".into(),
            bundle.to_string_lossy().into_owned(),
            "--all".into(),
        ],
        None,
        &stage.join("bundle.log"),
        capture,
    )
    .await?;
    File::open(&bundle)
        .and_then(|file| file.sync_all())
        .map_err(io)?;
    let restored = stage.join("restored.git");
    run_owned(
        service,
        operation,
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
        capture,
    )
    .await?;
    run_owned(
        service,
        operation,
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
        capture,
    )
    .await?;
    run(
        service,
        operation,
        &restored,
        &["fsck", "--full", "--no-reflogs"],
        None,
        &stage.join("fsck.log"),
        capture,
    )
    .await?;
    let restored_refs = stage.join("restored-refs");
    run(
        service,
        operation,
        &restored,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
        None,
        &restored_refs,
        capture,
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
    storage::atomic_json(&stage.join("verified-refs.json"), &refs)?;
    lfs::preserve(service, operation, snapshot, &stage, &restored, capture).await?;
    storage::sync_dir(&stage)?;
    Ok(bundle)
}

fn validate_oid(oid: &str) -> Result<()> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(corrupt("Invalid Git object identity"));
    }
    Ok(())
}

async fn run_owned(
    service: &WorkspaceService,
    operation: OperationId,
    cwd: &Path,
    args: &[String],
    input: Option<&Path>,
    output: &Path,
    capture: &dyn OutputCapture,
) -> Result<()> {
    run(
        service,
        operation,
        cwd,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        input,
        output,
        capture,
    )
    .await
}

async fn run(
    service: &WorkspaceService,
    operation: OperationId,
    cwd: &Path,
    args: &[&str],
    input: Option<&Path>,
    output: &Path,
    capture: &dyn OutputCapture,
) -> Result<()> {
    let mut command = tokio::process::Command::from(checkout::git::git(Some(cwd), args));
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0");
    // All operations are local. The only file transport exception is the exact
    // owned bundle used for the restoration above, never an arbitrary source.
    command.env("GIT_ALLOW_PROTOCOL", "file");
    if let Some(input) = input {
        command.stdin(Stdio::from(File::open(input).map_err(io)?));
    }
    let destination = storage::private_file(output, true)?;
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
                    if service.inspect_closeout(operation)?.stage == CloseoutStage::Revoked {
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
    if !status?.success() {
        return Err(issue(
            IssueCode::PreservationIncomplete,
            "Offline Git operation failed; inspect retained execution diagnostics",
        ));
    }
    Ok(())
}

async fn drain(
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
