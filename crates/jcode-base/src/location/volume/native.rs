use super::*;
use anyhow::{Context, ensure};

pub(super) struct NativeVolumes;
impl VolumeEnvironment for NativeVolumes {
    fn mounted(&self) -> Result<Vec<VolumeInfo>> {
        #[cfg(target_os = "macos")]
        {
            macos::mounted()?
                .iter()
                .map(|mount| macos::inspect(mount))
                .collect()
        }
        #[cfg(not(target_os = "macos"))]
        Err(LocationError::new(
            LocationIssue::Unsupported,
            Path::new(""),
            "workspace volume discovery requires the macOS adapter",
        ))
    }
    fn containing(&self, existing: &Path) -> Result<VolumeInfo> {
        #[cfg(target_os = "macos")]
        {
            let mount = macos::containing_mount(existing)?;
            let info = macos::inspect(&mount)?;
            macos::require_same_device(existing, &info.mount)?;
            Ok(info)
        }
        #[cfg(not(target_os = "macos"))]
        Err(LocationError::new(
            LocationIssue::Unsupported,
            existing,
            "workspace volume binding requires the macOS adapter",
        ))
    }
}

pub(super) fn verify_archive_mount(mount: &Path, uuid: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // Preserve the archive's original UUID/mount contract. No workspace
        // default, writable-policy or retained-output migration is applied here.
        let value = macos::disk_info(mount)?;
        ensure!(
            value["VolumeUUID"]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case(uuid))
                && value["MountPoint"]
                    .as_str()
                    .is_some_and(|v| Path::new(v) == mount),
            "Output archive volume identity does not match configuration"
        );
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let device = std::fs::metadata(Path::new("/dev/disk/by-uuid").join(uuid))?;
        ensure!(
            device.rdev() == std::fs::metadata(mount)?.dev(),
            "Output archive volume identity does not match configuration"
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("Verified archive-volume placement is unsupported on this platform");
    Ok(())
}

pub(crate) fn available_bytes(path: &Path) -> anyhow::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: name is NUL-terminated and stats points to writable statvfs storage.
        if unsafe { libc::statvfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful statvfs initialized the entire structure.
        let stats = unsafe { stats.assume_init() };
        Ok(
            (u128::from(stats.f_bavail) * u128::from(stats.f_frsize)).min(u128::from(u64::MAX))
                as u64,
        )
    }
    #[cfg(not(unix))]
    anyhow::bail!("Reserve-aware output storage is unsupported on this platform")
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::io::{Seek, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::process::{Command, Stdio};

    pub(super) fn disk_info(mount: &Path) -> anyhow::Result<serde_json::Value> {
        let info = Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist"])
            .arg(mount)
            .stdin(Stdio::null())
            .output()?;
        ensure!(
            info.status.success(),
            "Cannot verify volume identity at {}",
            mount.display()
        );
        // A file-backed converter input avoids a pipe-capacity deadlock. This is
        // private temporary metadata, not persistent volume or archive state.
        let mut input = tempfile::tempfile()?;
        input.write_all(&info.stdout)?;
        input.rewind()?;
        let converted = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "-"])
            .stdin(input)
            .output()?;
        ensure!(
            converted.status.success(),
            "Invalid volume identity response"
        );
        serde_json::from_slice(&converted.stdout).context("Invalid volume property list")
    }

    fn mount_name(stats: &libc::statfs) -> Result<PathBuf> {
        // SAFETY: statfs's fixed mount-name array is NUL terminated by the OS.
        let bytes = unsafe { CStr::from_ptr(stats.f_mntonname.as_ptr()) }.to_bytes();
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        validate_absolute(&path)?;
        Ok(path)
    }

    pub(super) fn containing_mount(path: &Path) -> Result<PathBuf> {
        let name =
            CString::new(path.as_os_str().as_bytes()).map_err(|e| LocationError::io(path, e))?;
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: a valid C path and writable statfs allocation are supplied.
        if unsafe { libc::statfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Err(LocationError::io(path, std::io::Error::last_os_error()));
        }
        // SAFETY: success initialized the structure.
        mount_name(&unsafe { stats.assume_init() })
    }

    pub(super) fn mounted() -> Result<Vec<PathBuf>> {
        // getfsstat copies into caller-owned storage, unlike getmntinfo's shared
        // mutable buffer. Retry if the mount table grows during the observation.
        loop {
            // SAFETY: null/zero requests the current count without writing.
            let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
            if count < 0 {
                return Err(LocationError::io(
                    Path::new("/"),
                    std::io::Error::last_os_error(),
                ));
            }
            let capacity =
                usize::try_from(count).map_err(|e| LocationError::io(Path::new("/"), e))? + 1;
            let size = capacity
                .checked_mul(std::mem::size_of::<libc::statfs>())
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| LocationError::io(Path::new("/"), "mount table is too large"))?;
            let mut buffer = Vec::<libc::statfs>::with_capacity(capacity);
            // SAFETY: buffer has capacity for size writable bytes. Length is
            // installed only after success and only for initialized entries.
            let copied = unsafe { libc::getfsstat(buffer.as_mut_ptr(), size, libc::MNT_NOWAIT) };
            if copied < 0 {
                return Err(LocationError::io(
                    Path::new("/"),
                    std::io::Error::last_os_error(),
                ));
            }
            let copied =
                usize::try_from(copied).map_err(|e| LocationError::io(Path::new("/"), e))?;
            if copied >= capacity {
                continue;
            }
            // SAFETY: successful getfsstat initialized exactly copied entries.
            unsafe {
                buffer.set_len(copied);
            }
            let mut mounts = Vec::new();
            for stats in buffer {
                // SAFETY: OS initializes and NUL terminates f_mntfromname.
                let device = unsafe { CStr::from_ptr(stats.f_mntfromname.as_ptr()) }.to_bytes();
                if device.starts_with(b"/dev/") {
                    mounts.push(mount_name(&stats)?);
                }
            }
            mounts.sort();
            mounts.dedup();
            return Ok(mounts);
        }
    }

    pub(super) fn require_same_device(path: &Path, mount: &Path) -> Result<()> {
        let a = std::fs::metadata(path).map_err(|e| LocationError::io(path, e))?;
        let b = std::fs::metadata(mount).map_err(|e| LocationError::io(mount, e))?;
        if a.dev() != b.dev() {
            return Err(LocationError::new(
                LocationIssue::WrongVolume,
                path,
                "path and verified mount refer to different devices",
            ));
        }
        Ok(())
    }

    pub(super) fn inspect(mount: &Path) -> Result<VolumeInfo> {
        let before = std::fs::metadata(mount).map_err(|e| LocationError::io(mount, e))?;
        let value = disk_info(mount).map_err(|e| LocationError::io(mount, e))?;
        let field = |key| {
            value[key].as_str().ok_or_else(|| {
                LocationError::new(
                    LocationIssue::Unsupported,
                    mount,
                    format!("missing {key} volume fact"),
                )
            })
        };
        let identity = VolumeIdentity::parse(field("VolumeUUID")?)?;
        let reported = Path::new(field("MountPoint")?)
            .canonicalize()
            .map_err(|e| LocationError::io(mount, e))?;
        let canonical = mount
            .canonicalize()
            .map_err(|e| LocationError::io(mount, e))?;
        if reported != canonical || containing_mount(&canonical)? != canonical {
            return Err(LocationError::new(
                LocationIssue::WrongVolume,
                mount,
                "selected path is not the reported mounted volume",
            ));
        }
        let after = std::fs::metadata(&canonical).map_err(|e| LocationError::io(mount, e))?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(LocationError::new(
                LocationIssue::ReplacedRoot,
                mount,
                "mount changed during inspection",
            ));
        }
        let boolean = |key| {
            value[key].as_bool().ok_or_else(|| {
                LocationError::new(
                    LocationIssue::Unsupported,
                    mount,
                    format!("missing {key} volume fact"),
                )
            })
        };
        Ok(VolumeInfo {
            identity,
            mount: canonical,
            label: field("VolumeName")?.to_owned(),
            internal: boolean("Internal")?,
            writable: boolean("WritableVolume")? && boolean("WritableMedia")?,
            available_bytes: available_bytes(mount).map_err(|e| LocationError::io(mount, e))?,
        })
    }
}
