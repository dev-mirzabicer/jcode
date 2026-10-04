//! macOS login-service supervision for one runtime namespace.
//!
//! One namespaced LaunchAgent starts the ordinary shared runtime at login and
//! restarts it after an unexpected exit. It is an availability adapter, not a
//! lifecycle owner: desired Stop/Start, quiescence and recovery stay with the
//! runtime journal. The job uses exit-based restart (`SuccessfulExit = false`),
//! so an intentional Stop (exit 0) is never undone, and it leaves surviving
//! native task process groups alone (`AbandonProcessGroup`).
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Set to the service label in the supervised runtime's environment.
pub const SUPERVISED_ENV: &str = "JCODE_RUNTIME_SUPERVISED";
/// Test and fixture override for where service definitions are written.
pub const LAUNCH_AGENTS_DIR_ENV: &str = "JCODE_LAUNCH_AGENTS_DIR";
/// Seconds launchd waits after SIGTERM before SIGKILL. It exceeds the
/// runtime's external-signal quiescence bound so checkpoints can publish.
pub const EXIT_TIMEOUT_SECONDS: u32 = 45;
pub const THROTTLE_INTERVAL_SECONDS: u32 = 10;
const LABEL_PREFIX: &str = "dev.jcode.runtime.";

/// Exact, reviewable definition of the service for one socket namespace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePlan {
    pub label: String,
    pub namespace: String,
    pub socket: PathBuf,
    pub definition: PathBuf,
    /// Stable launcher channel, never a transient build output.
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub log: PathBuf,
    pub exit_timeout_seconds: u32,
    pub throttle_interval_seconds: u32,
    /// Digest of the rendered definition; confirmation binds to it.
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceStatus {
    pub label: String,
    pub definition: PathBuf,
    pub installed: bool,
    /// Whether the installed definition equals a reviewed plan byte for byte.
    /// `None` when the status was read without a plan to compare against.
    #[serde(default)]
    pub current: Option<bool>,
    pub loaded: bool,
    pub pid: Option<u32>,
    pub last_exit: Option<String>,
}

fn namespace_for_socket(socket: &Path) -> Result<(PathBuf, String)> {
    ensure!(
        socket.is_absolute(),
        "Runtime socket must have an absolute namespace"
    );
    let parent = socket
        .parent()
        .context("Runtime socket has no parent")?
        .canonicalize()
        .context("Runtime socket directory is unavailable")?;
    let socket = parent.join(socket.file_name().context("Runtime socket has no name")?);
    let namespace = format!(
        "{:x}",
        Sha256::digest(socket.as_os_str().as_encoded_bytes())
    );
    Ok((socket, namespace))
}

pub fn label_for_socket(socket: &Path) -> Result<String> {
    let (_, namespace) = namespace_for_socket(socket)?;
    Ok(format!("{LABEL_PREFIX}{}", &namespace[..16]))
}

fn launch_agents_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(LAUNCH_AGENTS_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::home_dir()
        .context("No home directory for LaunchAgents")?
        .join("Library")
        .join("LaunchAgents"))
}

/// Deliberate service PATH: the installer's existing absolute entries, in
/// order, without duplicates. The review shows it before confirmation.
pub fn service_path(path: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    std::env::split_paths(path)
        .filter(|entry| entry.is_absolute() && entry.is_dir())
        .filter(|entry| seen.insert(entry.clone()))
        .map(|entry| entry.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

/// Build the plan from the installing environment. Only the variables that
/// select this runtime's namespace and state root are carried; credentials
/// stay with their existing stores and are never copied into the definition.
pub fn plan(socket: &Path, program: &Path, path: &str) -> Result<ServicePlan> {
    ensure!(
        cfg!(target_os = "macos"),
        "Login-service supervision is supported on macOS only"
    );
    let (socket, namespace) = namespace_for_socket(socket)?;
    ensure!(
        program.is_absolute(),
        "Service launcher must be an absolute path"
    );
    let metadata = std::fs::metadata(program)
        .with_context(|| format!("Service launcher {} is unavailable", program.display()))?;
    ensure!(metadata.is_file(), "Service launcher is not a file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "Service launcher is not executable"
        );
    }
    let label = format!("{LABEL_PREFIX}{}", &namespace[..16]);
    let mut environment = BTreeMap::new();
    let path = service_path(path);
    ensure!(!path.is_empty(), "Service PATH would be empty");
    environment.insert("PATH".to_string(), path);
    environment.insert(
        "JCODE_SOCKET".to_string(),
        socket.to_string_lossy().into_owned(),
    );
    environment.insert(SUPERVISED_ENV.to_string(), label.clone());
    environment.insert("JCODE_DEFERRED_AUTH_BOOTSTRAP".to_string(), "1".to_string());
    // Path selectors only: they decide the namespace, durable state, config
    // and credential-store locations exactly as the installing shell does.
    // TMPDIR is deliberately not carried: launchd gives the agent the user's
    // standard temporary directory, which is where an ordinary runtime keeps
    // its daemon lock. An installer with a private TMPDIR (a sandboxed tool
    // shell) must not move the service onto a different lock and so let it
    // compete with the runtime it is meant to take over.
    for key in [
        "HOME",
        "JCODE_HOME",
        "JCODE_RUNTIME_DIR",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.is_empty()
        {
            environment.insert(key.to_string(), value);
        }
    }
    let log = crate::storage::logs_dir()?.join(format!("{label}.log"));
    let mut plan = ServicePlan {
        definition: launch_agents_dir()?.join(format!("{label}.plist")),
        label,
        namespace,
        socket: socket.clone(),
        program: program.to_path_buf(),
        arguments: vec![
            "serve".into(),
            "--socket".into(),
            socket.to_string_lossy().into_owned(),
        ],
        environment,
        log,
        exit_timeout_seconds: EXIT_TIMEOUT_SECONDS,
        throttle_interval_seconds: THROTTLE_INTERVAL_SECONDS,
        digest: String::new(),
    };
    plan.digest = format!("{:x}", Sha256::digest(render(&plan).as_bytes()));
    Ok(plan)
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Rendered property list. Deterministic, so the digest is stable.
pub fn render(plan: &ServicePlan) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    let string = |out: &mut String, key: &str, value: &str| {
        out.push_str(&format!(
            "  <key>{}</key>\n  <string>{}</string>\n",
            key,
            escape(value)
        ));
    };
    string(&mut out, "Label", &plan.label);
    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for argument in std::iter::once(plan.program.to_string_lossy().into_owned())
        .chain(plan.arguments.iter().cloned())
    {
        out.push_str(&format!("    <string>{}</string>\n", escape(&argument)));
    }
    out.push_str("  </array>\n  <key>EnvironmentVariables</key>\n  <dict>\n");
    for (key, value) in &plan.environment {
        out.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            escape(key),
            escape(value)
        ));
    }
    out.push_str("  </dict>\n");
    out.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    out.push_str(
        "  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n",
    );
    out.push_str("  <key>AbandonProcessGroup</key>\n  <true/>\n");
    out.push_str("  <key>ProcessType</key>\n  <string>Standard</string>\n");
    out.push_str(&format!(
        "  <key>ExitTimeOut</key>\n  <integer>{}</integer>\n",
        plan.exit_timeout_seconds
    ));
    out.push_str(&format!(
        "  <key>ThrottleInterval</key>\n  <integer>{}</integer>\n",
        plan.throttle_interval_seconds
    ));
    string(&mut out, "StandardOutPath", &plan.log.to_string_lossy());
    string(&mut out, "StandardErrorPath", &plan.log.to_string_lossy());
    out.push_str("</dict>\n</plist>\n");
    out
}

#[cfg(unix)]
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn domain() -> String {
    String::new()
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("/bin/launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .context("Run launchctl")
}

/// Write the reviewed definition and load it into the user's login domain.
/// The confirmation digest must equal the freshly computed plan.
pub fn install(plan: &ServicePlan, confirmed_digest: &str) -> Result<ServiceStatus> {
    ensure!(
        plan.digest == confirmed_digest,
        "Service plan changed since review; review it again"
    );
    let rendered = render(plan);
    ensure!(
        format!("{:x}", Sha256::digest(rendered.as_bytes())) == plan.digest,
        "Service plan digest does not match its rendering"
    );
    let current = status_for(plan)?;
    if current.loaded && current.current == Some(true) {
        return Ok(current);
    }
    ensure!(
        !current.loaded,
        "A different definition for {} is loaded; uninstall it before installing this plan",
        plan.label
    );
    let parent = plan
        .definition
        .parent()
        .context("Service definition has no directory")?;
    std::fs::create_dir_all(parent)?;
    if let Some(log_parent) = plan.log.parent() {
        crate::storage::ensure_dir(log_parent)?;
    }
    let temp = parent.join(format!(".{}.plist.{}", plan.label, std::process::id()));
    std::fs::write(&temp, rendered.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&temp, &plan.definition)?;
    std::fs::File::open(parent)?.sync_all()?;
    let output = launchctl(&["bootstrap", &domain(), &plan.definition.to_string_lossy()])?;
    ensure!(
        output.status.success(),
        "launchctl bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    status_for(plan)
}

/// Unload and remove the definition. The caller ensures the supervised
/// runtime is stopped or accepts that launchd delivers SIGTERM to it.
pub fn uninstall(socket: &Path) -> Result<ServiceStatus> {
    let label = label_for_socket(socket)?;
    let definition = launch_agents_dir()?.join(format!("{label}.plist"));
    let loaded = launchctl(&["print", &format!("{}/{label}", domain())])?
        .status
        .success();
    if loaded {
        let output = launchctl(&["bootout", &format!("{}/{label}", domain())])?;
        ensure!(
            output.status.success(),
            "launchctl bootout failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    match std::fs::remove_file(&definition) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(ServiceStatus {
        label,
        definition,
        installed: false,
        current: None,
        loaded: false,
        pid: None,
        last_exit: None,
    })
}

fn parse_print(text: &str) -> (Option<u32>, Option<String>) {
    let mut pid = None;
    let mut last_exit = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("pid = ") {
            pid = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("last exit code = ") {
            last_exit = Some(value.trim().to_string());
        }
    }
    (pid, last_exit)
}

pub fn status_for(plan: &ServicePlan) -> Result<ServiceStatus> {
    let mut status = status(&plan.socket)?;
    status.current = Some(
        std::fs::read(&plan.definition)
            .map(|bytes| bytes == render(plan).as_bytes())
            .unwrap_or(false),
    );
    Ok(status)
}

pub fn status(socket: &Path) -> Result<ServiceStatus> {
    let label = label_for_socket(socket)?;
    let definition = launch_agents_dir()?.join(format!("{label}.plist"));
    let installed = definition.exists();
    let (loaded, pid, last_exit) = if cfg!(target_os = "macos") {
        let output = launchctl(&["print", &format!("{}/{label}", domain())])?;
        if output.status.success() {
            let (pid, last_exit) = parse_print(&String::from_utf8_lossy(&output.stdout));
            (true, pid, last_exit)
        } else {
            (false, None, None)
        }
    } else {
        (false, None, None)
    };
    Ok(ServiceStatus {
        label,
        definition,
        installed,
        current: None,
        loaded,
        pid,
        last_exit,
    })
}

/// True when this namespace's login service is installed and loaded, so
/// availability belongs to it rather than to an unmanaged spawn.
pub fn registered_for_socket(socket: &Path) -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let status = status(socket)?;
    Ok(status.installed && status.loaded)
}

/// Ask launchd to run the registered job if it is not already running. This
/// never kills a running instance (no `-k`).
pub fn kickstart(socket: &Path) -> Result<()> {
    let label = label_for_socket(socket)?;
    let output = launchctl(&["kickstart", &format!("{}/{label}", domain())])?;
    ensure!(
        output.status.success(),
        "launchctl kickstart failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_deterministic_namespaced_and_credential_free() -> Result<()> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let _env = crate::storage::lock_test_env();
        let root = tempfile::tempdir()?;
        let program = root.path().join("jcode");
        std::fs::write(&program, b"#!/bin/sh\n")?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
        }
        let socket = root.path().join("a.sock");
        let path = format!(
            "{}:/nonexistent:relative:{}",
            root.path().display(),
            root.path().display()
        );
        // A private installer TMPDIR (for example a sandboxed tool shell) must
        // not move the service onto another daemon-lock directory.
        let saved_tmpdir = std::env::var_os("TMPDIR");
        crate::env::set_var("TMPDIR", root.path());
        let first = plan(&socket, &program, &path);
        match saved_tmpdir {
            Some(value) => crate::env::set_var("TMPDIR", value),
            None => crate::env::remove_var("TMPDIR"),
        }
        let first = first?;
        assert!(!first.environment.contains_key("TMPDIR"));
        let again = plan(&socket, &program, &path)?;
        assert_eq!(first, again);
        assert_eq!(first.environment["PATH"], root.path().display().to_string());
        assert_eq!(first.environment[SUPERVISED_ENV], first.label);
        assert!(
            !first
                .environment
                .keys()
                .any(|key| key.contains("KEY") || key.contains("TOKEN"))
        );
        let other = plan(&root.path().join("b.sock"), &program, &path)?;
        assert_ne!(first.label, other.label, "each socket has its own service");
        let rendered = render(&first);
        assert!(rendered.contains("<key>SuccessfulExit</key>\n    <false/>"));
        assert!(rendered.contains("<key>AbandonProcessGroup</key>\n  <true/>"));
        assert!(
            install(&first, "stale").is_err(),
            "confirmation binds to the digest"
        );
        Ok(())
    }

    #[test]
    fn launchctl_print_parsing_reads_pid_and_last_exit() {
        let (pid, exit) = parse_print("state = running\n\tpid = 4242\n\tlast exit code = 0\n");
        assert_eq!(pid, Some(4242));
        assert_eq!(exit.as_deref(), Some("0"));
    }
}
