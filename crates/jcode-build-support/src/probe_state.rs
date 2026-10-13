//! Private state for running a candidate binary before it is trusted.
//!
//! A candidate run against the real home can migrate durable stores, upgrade
//! the instruction seed, create sessions or settle real deliveries before it
//! replaces the running daemon. If the reload then fails, the running daemon
//! is left on state it may not be able to open. Every probe therefore runs in
//! a disposable home: it proves the binary starts and speaks the protocol,
//! and only the activated daemon's own startup touches real state.
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The only inherited variables a probe sees: what a process needs to run
/// tools such as git. Nothing that names Jcode state, a socket, a session or
/// a supervising service reaches it.
const INHERITED: &[&str] = &[
    "PATH", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TERM",
];

/// Directories of the private state root, each a separate fallback so no
/// HOME- or XDG-derived path leaves it.
const DIRECTORIES: &[&str] = &[
    "home", "jcode", "runtime", "config", "cache", "data", "state", "tmp", "work",
];

pub(crate) struct ProbeState {
    root: tempfile::TempDir,
}

impl ProbeState {
    pub(crate) fn new() -> Result<Self> {
        // A short base keeps Unix-domain socket paths under the macOS limit.
        let base = if cfg!(unix) && Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let root = tempfile::Builder::new()
            .prefix("jcode-probe-")
            .tempdir_in(base)?;
        for name in DIRECTORIES {
            let path = root.path().join(name);
            std::fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(Self { root })
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The probe's own working directory, never a real project.
    pub(crate) fn work_dir(&self) -> PathBuf {
        self.path("work")
    }

    /// A command for `binary` that sees only the private state root.
    pub(crate) fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command.env_clear();
        for key in INHERITED {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let env: [(&str, &str); 9] = [
            ("HOME", "home"),
            ("JCODE_HOME", "jcode"),
            ("JCODE_RUNTIME_DIR", "runtime"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("TMPDIR", "tmp"),
        ];
        for (key, directory) in env {
            command.env(key, self.path(directory));
        }
        // A probe is not an installation: it sends no usage telemetry, which a
        // fresh home would otherwise count as a new install on every reload.
        command
            .env("JCODE_NON_INTERACTIVE", "1")
            .env("JCODE_NO_TELEMETRY", "1")
            .current_dir(self.work_dir());
        command
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }
}

/// Whether a command's environment is confined to `state`: every variable
/// is inherited from the allowlist or points into the state root.
#[cfg(test)]
pub(crate) fn confined(command: &Command, state: &ProbeState) -> bool {
    command.get_envs().all(|(key, value)| {
        let Some(value) = value else { return true };
        INHERITED
            .iter()
            .any(|allowed| std::ffi::OsStr::new(allowed) == key)
            || key == "JCODE_NON_INTERACTIVE"
            || key == "JCODE_NO_TELEMETRY"
            || Path::new(value).starts_with(state.root())
    })
}
