//! Shared-server lifecycle hooks usable from lower layers.
//!
//! The actual "spawn a shared jcode server" logic lives in the CLI command
//! layer (`cli::dispatch::spawn_server`) because it depends on CLI types like
//! `ProviderChoice` and on argument-driven bootstrap. Lower layers such as the
//! TUI reconnect loop still need to (a) check whether a shared server is
//! reachable and (b) request a replacement server when a reload stalls.
//!
//! To avoid a `tui -> cli` dependency, the CLI registers a default spawner here
//! at startup (mirroring the `register_permission_notifier` /
//! `register_api_key_fallback_resolver` inversion pattern). Consumers call
//! [`is_running`] and [`spawn_default_server`] without knowing about `cli`.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

type ServerSpawner = Box<
    dyn Fn(ServerEnvironment) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync,
>;

#[derive(Clone, Copy)]
pub enum ServerEnvironment {
    Inherit,
    ConfiguredDelegation,
}

impl ServerEnvironment {
    pub fn apply(self, command: &mut std::process::Command) {
        if matches!(self, Self::Inherit) {
            return;
        }
        // Keep OS execution and explicit Jcode namespace identity. Provider/MCP
        // credentials come from persistent host configuration, not arbitrary
        // exports of the standalone parent that happened to start the host.
        const OS_AND_NAMESPACE: &[&str] = &[
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "COLORTERM",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "PROGRAMDATA",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "JCODE_HOME",
            "JCODE_RUNTIME_DIR",
            "JCODE_SOCKET",
            "JCODE_NO_TELEMETRY",
            "DO_NOT_TRACK",
        ];
        command.env_clear();
        for key in OS_AND_NAMESPACE {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
}

static DEFAULT_SERVER_SPAWNER: OnceLock<ServerSpawner> = OnceLock::new();

/// Register the default shared-server spawner.
///
/// Called once at startup by the CLI layer, which owns the provider-bootstrap
/// logic. Subsequent calls are ignored.
pub fn register_default_server_spawner(spawner: ServerSpawner) {
    let _ = DEFAULT_SERVER_SPAWNER.set(spawner);
}

/// Returns true if a shared server is currently reachable on the default socket.
pub async fn is_running() -> bool {
    let socket = crate::server::socket_path();
    crate::server::is_server_ready(&socket).await || crate::server::has_live_listener(&socket).await
}

/// Spawn a replacement shared server using the registered default spawner.
///
/// Returns an error if no spawner has been registered (e.g. in a context that
/// never initialized the CLI startup hooks).
pub async fn spawn_default_server() -> Result<()> {
    match DEFAULT_SERVER_SPAWNER.get() {
        Some(spawner) => spawner(ServerEnvironment::Inherit).await,
        None => anyhow::bail!("no default server spawner registered"),
    }
}

pub async fn spawn_delegation_server() -> Result<()> {
    match DEFAULT_SERVER_SPAWNER.get() {
        Some(spawner) => spawner(ServerEnvironment::ConfiguredDelegation).await,
        None => anyhow::bail!("no default server spawner registered"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delegation_host_environment_excludes_parent_exports_and_keeps_namespace() {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut command = std::process::Command::new("fixture");
        command.env("SYNTHETIC_PARENT_SECRET", "not forwarded");
        ServerEnvironment::ConfiguredDelegation.apply(&mut command);
        let values = command
            .get_envs()
            .collect::<std::collections::HashMap<_, _>>();
        assert!(!values.contains_key(std::ffi::OsStr::new("SYNTHETIC_PARENT_SECRET")));
        assert_eq!(
            values
                .get(std::ffi::OsStr::new("JCODE_HOME"))
                .unwrap()
                .unwrap(),
            std::env::var_os("JCODE_HOME").unwrap()
        );
        let mut ordinary = std::process::Command::new("fixture");
        ordinary.env("SYNTHETIC_PARENT_SECRET", "retained");
        ServerEnvironment::Inherit.apply(&mut ordinary);
        assert!(
            ordinary
                .get_envs()
                .any(|(key, _)| key == "SYNTHETIC_PARENT_SECRET")
        );
    }
}
