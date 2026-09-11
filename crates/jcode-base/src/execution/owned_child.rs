//! In-process ownership of a freshly spawned private Unix process group.
//! This is not a PID-based recovery or service-management interface.
use super::process::ProcessIdentity;
use anyhow::{Context, Result, ensure};
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

pub struct OwnedChild {
    child: Child,
    identity: Option<ProcessIdentity>,
}
impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command.process_group(0).kill_on_drop(true);
        let child = command.spawn()?;
        let mut owned = Self {
            child,
            identity: None,
        };
        let identity =
            ProcessIdentity::capture(owned.child.id().context("Spawned child has no PID")?)?;
        ensure!(
            identity.pid == identity.group,
            "Spawned child did not own its process group"
        );
        owned.identity = Some(identity);
        Ok(owned)
    }
    pub fn stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }
    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }
    pub fn stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.child.wait().await?;
        // Reaping releases the PID. Never defer disarming across an await.
        self.identity = None;
        Ok(status)
    }
    pub async fn stop(&mut self) -> Result<ExitStatus> {
        let Some(identity) = self.identity.as_ref() else {
            return self.wait().await;
        };
        let group = identity.group;
        let mut failure = None;
        if crate::platform::process_group_has_live_members(group).await? {
            if let Err(error) = identity.signal_group(libc::SIGTERM) {
                failure = Some(error);
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            if crate::platform::process_group_has_live_members(group).await?
                && let Err(error) = identity.signal_group(libc::SIGKILL)
            {
                failure = Some(error);
            }
        }
        // Preserve the unreaped leader and group identity until all owned work
        // actually stops. A stop request is not a quiescence acknowledgement.
        while crate::platform::process_group_has_live_members(group).await? {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let status = self.wait().await?;
        if let Some(error) = failure {
            return Err(
                error.context("Process group stopped, but its cancellation encountered an error")
            );
        }
        Ok(status)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(identity) = &self.identity {
            let _ = identity.signal_group(libc::SIGKILL);
        } else if let Some(pid) = self.child.id() {
            // Bootstrap failure: this newly spawned, unreaped Child still pins
            // the private process group created by spawn. No persisted PID is used.
            let _ = crate::platform::signal_detached_process_group(pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    #[tokio::test]
    async fn stop_waits_for_the_private_group_and_does_not_undo_effects() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut command = Command::new("bash");
        command.current_dir(directory.path()).args(["-c", "printf before > before; trap '' TERM; (trap '' TERM; sleep 30; printf late > late) & echo ready; wait"])
            .stdout(std::process::Stdio::piped());
        let mut child = OwnedChild::spawn(&mut command)?;
        let group = child.identity.as_ref().unwrap().group;
        let mut output = BufReader::new(child.stdout().unwrap()).lines();
        assert_eq!(output.next_line().await?.as_deref(), Some("ready"));
        let status = tokio::time::timeout(Duration::from_secs(5), child.stop()).await??;
        assert!(!status.success());
        assert!(!crate::platform::process_group_has_live_members(group).await?);
        assert_eq!(std::fs::read(directory.path().join("before"))?, b"before");
        assert!(!directory.path().join("late").exists());
        Ok(())
    }
}
