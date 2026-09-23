use super::*;
use crate::execution::{ExecutionStore, owned_child::OwnedChild};
use crate::session::Session;
use jcode_tool_core::{OutputCapture, OutputStream};
use std::io::BufRead;

impl WorkspaceService {
    /// Install the admission fence before observing existing owners. This does
    /// not stop them. A nonempty report is a blocker, not a request to kill work.
    pub async fn prepare_closeout_work(
        &self,
        operation: OperationId,
        expected: Revision,
        session_root: &Path,
        execution: &ExecutionStore,
        executor_cwd: &Path,
        capture: &dyn OutputCapture,
    ) -> Result<(CloseoutRecord, CloseoutWorkReport)> {
        let _operation = self.closeout_lease(operation)?;
        let fenced = self.fence_closeout(operation, expected)?;
        let stored = load(&self.connection()?, operation)?;
        let mut findings = Vec::new();
        let _exclusive = match self.acquire_binding(&stored.binding) {
            Ok(lease) => Some(lease),
            Err(error) if error.code == IssueCode::Busy => {
                findings.push(finding(
                    CloseoutWorkKind::PhysicalLease,
                    stored.record.spec.location.to_string(),
                    error.to_string(),
                ));
                None
            }
            Err(error) => return Err(error),
        };
        self.observe_closeout_work(
            &stored,
            session_root,
            execution,
            executor_cwd,
            &mut findings,
        )?;
        match external_work(self, operation, stored.binding.observed_path(), capture).await {
            Ok(external) => findings.extend(external),
            Err(error) => findings.push(finding(
                CloseoutWorkKind::Unknown,
                "external-process-inspection",
                error.to_string(),
            )),
        }
        let report = CloseoutWorkReport {
            operation,
            observed_at: chrono::Utc::now().to_rfc3339(),
            findings,
        };
        // Observation is not approval, and time is not a lease. Final removal
        // re-runs these owners while holding exclusive physical ownership.
        Ok((fenced, report))
    }

    fn observe_closeout_work(
        &self,
        stored: &StoredCloseout,
        session_root: &Path,
        execution: &ExecutionStore,
        executor_cwd: &Path,
        findings: &mut Vec<CloseoutWorkFinding>,
    ) -> Result<()> {
        let root = stored.binding.observed_path();
        if touches(executor_cwd, root)? || touches(&std::env::current_dir().map_err(io)?, root)? {
            findings.push(finding(
                CloseoutWorkKind::Executor,
                "executor-cwd",
                "The closeout executor still uses this checkout as its effective working directory",
            ));
        }
        let inputs = crate::primary_input::PrimaryInputStore::new(
            self.root
                .parent()
                .ok_or_else(|| corrupt("Catalog has no state root"))?,
        );
        let sessions = session_root.join("sessions");
        if sessions.try_exists().map_err(io)? {
            for entry in std::fs::read_dir(&sessions).map_err(io)? {
                let path = entry.map_err(io)?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| corrupt("Session filename identity is unavailable"))?;
                let session = match Session::load_startup_stub_in(session_root, id) {
                    Ok(session) => session,
                    Err(error) => {
                        findings.push(finding(
                            CloseoutWorkKind::Unknown,
                            id,
                            format!("Session location cannot be inspected: {error}"),
                        ));
                        continue;
                    }
                };
                let affected = session
                    .working_dir
                    .as_deref()
                    .map(|cwd| touches(Path::new(cwd), root))
                    .transpose()?
                    .unwrap_or(false);
                if affected {
                    match execution.session_has_live_activity(id) {
                        Ok(true) => findings.push(finding(
                            CloseoutWorkKind::Session,
                            id,
                            "Primary or isolated child has a live turn in the checkout",
                        )),
                        Ok(false) => {}
                        Err(error) => findings.push(finding(
                            CloseoutWorkKind::Unknown,
                            id,
                            format!("Activity ownership cannot be established: {error}"),
                        )),
                    }
                    match inputs.pending_in(session_root, id) {
                        Ok(pending) => {
                            for input in pending {
                                findings.push(finding(
                                    CloseoutWorkKind::PendingInput,
                                    input.id.to_string(),
                                    format!("Accepted input is pending for Session {id}"),
                                ));
                            }
                        }
                        Err(error) => findings.push(finding(
                            CloseoutWorkKind::Unknown,
                            id,
                            format!("Input ownership cannot be established: {error}"),
                        )),
                    }
                }
                for change in self.pending_location_changes(id)? {
                    if affected || touches(&change.input.cwd, root)? {
                        findings.push(finding(
                            CloseoutWorkKind::PendingControl,
                            change.operation.to_string(),
                            format!("Session {id} has a pending location operation"),
                        ));
                    }
                }
            }
        }
        for run in execution.unresolved_runs().map_err(io)? {
            let invocation = execution.invocation_input(&run.id).map_err(io)?;
            let native_cwd = execution.command_working_directory(&run.id).map_err(io)?;
            if native_cwd
                .as_deref()
                .map(|cwd| touches(cwd, root))
                .transpose()?
                .unwrap_or(false)
                || invocation
                    .working_dir
                    .as_deref()
                    .map(|cwd| touches(cwd, root))
                    .transpose()?
                    .unwrap_or(false)
            {
                findings.push(finding(
                    CloseoutWorkKind::Execution,
                    run.id,
                    format!(
                        "{} is {:?}; original cwd remains bound to this checkout",
                        invocation.tool, run.state
                    ),
                ));
            }
        }
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT o.id,o.kind FROM operations o JOIN operation_targets t ON t.operation=o.id WHERE t.target=?1 AND o.id!=?2 AND o.state IN ('pending','recovery_required')").map_err(io)?;
        for row in query
            .query_map(
                params![
                    stored.record.spec.location.to_string(),
                    stored.record.operation.to_string()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(io)?
        {
            let (id, kind) = row.map_err(io)?;
            findings.push(finding(
                CloseoutWorkKind::PendingControl,
                id,
                format!("Unfinished catalog operation {kind} references the checkout"),
            ));
        }
        Ok(())
    }
}

fn touches(path: &Path, root: &Path) -> Result<bool> {
    Ok(crate::location::native_files::resolve_target(path)
        .map_err(io)?
        .starts_with(root))
}
fn finding(
    kind: CloseoutWorkKind,
    identity: impl Into<String>,
    detail: impl Into<String>,
) -> CloseoutWorkFinding {
    CloseoutWorkFinding {
        kind,
        identity: identity.into(),
        detail: detail.into(),
    }
}

#[cfg(target_os = "macos")]
async fn external_work(
    service: &WorkspaceService,
    operation: OperationId,
    root: &Path,
    capture: &dyn OutputCapture,
) -> Result<Vec<CloseoutWorkFinding>> {
    let directory = service
        .root
        .join("closeout-work")
        .join(operation.to_string());
    storage::private_dir(&directory)?;
    let request = RequestId::new();
    let output = directory.join(format!("{request}.stdout"));
    let diagnostics = directory.join(format!("{request}.stderr"));
    let stdout_file = storage::private_file(&output, true)?;
    let stderr_file = storage::private_file(&diagnostics, true)?;
    let mut command = tokio::process::Command::new("/usr/sbin/lsof");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // +D does not follow symlinks or cross mounted filesystems without -x.
    command.args(["-n", "-P", "-F0pcfn", "+D"]).arg(root);
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
            .ok_or_else(|| corrupt("Process inspection has no PID"))?,
    ) {
        child.stop().await.map_err(io)?;
        capture.finish_process(&ticket).map_err(io)?;
        return Err(io(error));
    }
    let mut stdout = child
        .stdout()
        .ok_or_else(|| corrupt("Process inspection stdout unavailable"))?;
    let mut stderr = child
        .stderr()
        .ok_or_else(|| corrupt("Process inspection stderr unavailable"))?;
    let wait = async {
        loop {
            tokio::select! {
                status = child.wait() => return status.map_err(io),
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if service.inspect_closeout(operation)?.stage == CloseoutStage::Revoked {
                        child.stop().await.map_err(io)?; return Err(issue(IssueCode::PermissionRequired, "Closeout authorization was revoked"));
                    }
                }
            }
        }
    };
    let (status, out, err) = tokio::join!(
        wait,
        git::drain(
            &mut stdout,
            capture,
            OutputStream::Stdout,
            Some(stdout_file)
        ),
        git::drain(
            &mut stderr,
            capture,
            OutputStream::Stderr,
            Some(stderr_file)
        )
    );
    capture.finish_process(&ticket).map_err(io)?;
    out?;
    err?;
    let status = status?;
    if std::fs::metadata(&diagnostics).map_err(io)?.len() != 0
        || !matches!(status.code(), Some(0 | 1))
    {
        return Err(issue(
            IssueCode::IncompleteCapture,
            "OS process inspection was incomplete; retained diagnostics must be resolved",
        ));
    }
    let mut findings = Vec::new();
    let mut pid = None;
    let mut command = String::new();
    let mut descriptor = String::new();
    for field in std::io::BufReader::new(std::fs::File::open(&output).map_err(io)?).split(0) {
        let field = field.map_err(io)?;
        let field = field.strip_prefix(b"\n").unwrap_or(&field);
        if field.is_empty() {
            continue;
        }
        let value = String::from_utf8_lossy(&field[1..]).into_owned();
        match field[0] {
            b'p' => {
                pid = Some(value.parse::<u32>().map_err(corrupt)?);
                command.clear();
                descriptor.clear();
            }
            b'c' => command = value,
            b'f' => descriptor = value,
            b'n' => {
                let pid = pid.ok_or_else(|| corrupt("Process-use observation has no owner"))?;
                if descriptor.is_empty() {
                    return Err(corrupt("Process-use observation has no descriptor"));
                }
                findings.push(finding(
                    CloseoutWorkKind::ExternalProcess,
                    format!("{pid}:{descriptor}"),
                    format!("{command} has an open checkout entry: {value}"),
                ));
            }
            _ => return Err(corrupt("Unknown OS process observation field")),
        }
    }
    if status.success() && findings.is_empty() {
        return Err(issue(
            IssueCode::IncompleteCapture,
            "OS reported file use but supplied no complete owner",
        ));
    }
    Ok(findings)
}

#[cfg(not(target_os = "macos"))]
async fn external_work(
    _: &WorkspaceService,
    _: OperationId,
    _: &Path,
    _: &dyn OutputCapture,
) -> Result<Vec<CloseoutWorkFinding>> {
    Err(issue(
        IssueCode::UnsupportedCapability,
        "Native external-work inspection is implemented for macOS",
    ))
}
