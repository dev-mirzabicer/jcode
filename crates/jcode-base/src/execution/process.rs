//! Persistable process identity. A bare PID is never sufficient for recovery.
#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub group: u32,
    boot: String,
    started: [u64; 2],
}
impl ProcessIdentity {
    pub fn capture(pid: u32) -> Result<Self> {
        ensure!(pid > 1, "Invalid owned process PID");
        #[cfg(target_os = "macos")]
        {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
            let size = std::mem::size_of::<libc::proc_bsdinfo>();
            // SAFETY: proc_pidinfo receives a writable buffer of its declared
            // size. Fields are read only after the kernel filled the full buffer.
            let written = unsafe {
                libc::proc_pidinfo(
                    i32::try_from(pid)?,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    i32::try_from(size)?,
                )
            };
            ensure!(
                written == i32::try_from(size)?,
                "Native process identity is unavailable"
            );
            let info = unsafe { info.assume_init() };
            ensure!(
                info.pbi_pid == pid,
                "Kernel returned a different process identity"
            );
            Ok(Self {
                pid,
                group: info.pbi_pgid,
                boot: boot_identity()?,
                started: [info.pbi_start_tvsec, info.pbi_start_tvusec],
            })
        }
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let fields: Vec<_> = stat
                .rsplit_once(") ")
                .context("Invalid process stat")?
                .1
                .split_whitespace()
                .collect();
            let group = fields.get(2).context("Missing process group")?.parse()?;
            let started = fields
                .get(19)
                .context("Missing process birth identity")?
                .parse()?;
            Ok(Self {
                pid,
                group,
                boot: boot_identity()?,
                started: [started, 0],
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        anyhow::bail!("Verified native process recovery is unsupported on this platform")
    }
    pub fn matches_live(&self) -> Result<bool> {
        if self.boot != boot_identity()? {
            return Ok(false);
        }
        match Self::capture(self.pid) {
            Ok(current) => Ok(current == *self),
            Err(error) => {
                if !crate::platform::is_process_running(self.pid) {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }
    }
    pub fn signal_group(&self, signal: i32) -> Result<()> {
        ensure!(
            self.pid == self.group && self.pid != std::process::id(),
            "Refusing to signal an unowned group or this Jcode runtime"
        );
        ensure!(
            self.matches_live()?,
            "Owned process identity changed; no process was signalled"
        );
        crate::platform::signal_detached_process_group(self.pid, signal)?;
        Ok(())
    }
}

fn boot_identity() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let name = c"kern.bootsessionuuid";
        let mut length = 0usize;
        // SAFETY: this read-only sysctl first supplies the required length, then
        // writes into a bounded initialized byte buffer. No system setting changes.
        ensure!(
            unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut length,
                    std::ptr::null_mut(),
                    0,
                )
            } == 0,
            "Boot identity is unavailable"
        );
        ensure!(
            (1..=1024).contains(&length),
            "Unexpected boot identity length"
        );
        let mut bytes = vec![0u8; length];
        ensure!(
            unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    &mut length,
                    std::ptr::null_mut(),
                    0,
                )
            } == 0,
            "Boot identity read failed"
        );
        bytes.truncate(length);
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(String::from_utf8(bytes)?)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("Boot identity is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_birth_or_boot_never_matches_an_existing_pid() -> Result<()> {
        let identity = ProcessIdentity::capture(std::process::id())?;
        assert!(identity.matches_live()?);
        let mut changed = identity.clone();
        changed.started[0] = changed.started[0].wrapping_add(1);
        assert!(!changed.matches_live()?);
        changed = identity.clone();
        changed.boot.push_str("-different");
        assert!(!changed.matches_live()?);
        assert!(identity.signal_group(libc::SIGTERM).is_err());
        Ok(())
    }

    #[test]
    fn only_the_matching_owned_group_can_be_signalled() -> Result<()> {
        use std::os::unix::process::CommandExt;
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(
            std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()?,
        );
        let identity = ProcessIdentity::capture(child.0.id())?;
        assert_eq!(identity.pid, identity.group);
        let mut wrong = identity.clone();
        wrong.started[0] = wrong.started[0].wrapping_add(1);
        assert!(wrong.signal_group(libc::SIGKILL).is_err());
        assert!(child.0.try_wait()?.is_none());
        identity.signal_group(libc::SIGKILL)?;
        child.0.wait()?;
        assert!(!identity.matches_live()?);
        Ok(())
    }
}
