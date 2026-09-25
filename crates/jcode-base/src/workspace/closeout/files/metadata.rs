//! Integrity of the metadata actually retained by the native copier. Inodes,
//! access/ctime and link counts are not portable metadata content.
use super::*;

pub(super) fn fingerprint(path: &Path, kind: CloseoutEntryKind) -> Result<String> {
    fingerprint_impl(path, kind, false)
}

/// Child removal legitimately changes a directory's mtime. Content metadata,
/// including ACLs, xattrs, flags, ownership, mode and birth time, must not change.
pub(in crate::workspace::closeout) fn removal_fingerprint(
    path: &Path,
    kind: CloseoutEntryKind,
) -> Result<String> {
    fingerprint_impl(path, kind, true)
}

#[cfg(target_os = "macos")]
fn fingerprint_impl(path: &Path, kind: CloseoutEntryKind, removing: bool) -> Result<String> {
    use crate::location::native_files::VerifiedDirectory;
    use std::ffi::{CString, c_void};
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut c_void;
        fn acl_to_text(acl: *mut c_void, len: *mut libc::ssize_t) -> *mut libc::c_char;
        fn acl_free(value: *mut c_void) -> libc::c_int;
    }
    struct AclAllocation(*mut c_void);
    impl Drop for AclAllocation {
        fn drop(&mut self) {
            // SAFETY: allocations come only from the paired macOS ACL APIs.
            unsafe {
                acl_free(self.0);
            }
        }
    }
    let parent = VerifiedDirectory::open(
        path.parent()
            .ok_or_else(|| corrupt("Metadata parent missing"))?
            .into(),
    )
    .map_err(io)?;
    let file = parent
        .open_preserved_entry(
            path.file_name()
                .ok_or_else(|| corrupt("Metadata leaf missing"))?,
            kind == CloseoutEntryKind::Symlink,
        )
        .map_err(io)?;
    let before = Witness::of(&file.metadata().map_err(io)?)?;
    let fd = file.as_raw_fd();
    // SAFETY: descriptor is retained; the first query writes nothing. The second
    // has exactly the allocated buffer capacity and its result is checked.
    let length = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    if length < 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    let mut names = vec![0u8; usize::try_from(length).map_err(corrupt)?];
    let read = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0) };
    if read != length {
        return Err(issue(
            IssueCode::Conflict,
            "Preserved attribute inventory changed",
        ));
    }
    let mut attributes = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let key = CString::new(name).map_err(corrupt)?;
        // SAFETY: valid descriptor and terminated key; query-only buffer.
        let length = unsafe { libc::fgetxattr(fd, key.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        if length < 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
        let length = usize::try_from(length).map_err(corrupt)?;
        let mut hash = Sha256::new();
        if name == b"com.apple.ResourceFork" {
            // Darwin permits positioned reads specifically for resource forks.
            let mut buffer = [0u8; 65536];
            let mut position = 0;
            while position < length {
                let count = buffer.len().min(length - position);
                let offset = u32::try_from(position).map_err(|_| {
                    issue(
                        IssueCode::PreservationIncomplete,
                        "Resource fork exceeds native positioned-read range",
                    )
                })?;
                // SAFETY: buffer is count bytes and remains live for the call.
                let read = unsafe {
                    libc::fgetxattr(
                        fd,
                        key.as_ptr(),
                        buffer.as_mut_ptr().cast(),
                        count,
                        offset,
                        0,
                    )
                };
                if read <= 0 || usize::try_from(read).map_err(corrupt)? > count {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Resource fork changed during verification",
                    ));
                }
                let read = usize::try_from(read).map_err(corrupt)?;
                hash.update(&buffer[..read]);
                position += read;
            }
        } else {
            // Ordinary attributes are atomic bounded filesystem values. Retain
            // one value at a time, never the complete tree's attributes.
            let mut value = vec![0u8; length];
            let read = unsafe {
                libc::fgetxattr(
                    fd,
                    key.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            if read < 0 {
                return Err(io(std::io::Error::last_os_error()));
            }
            if usize::try_from(read).map_err(corrupt)? != length {
                return Err(issue(
                    IssueCode::Conflict,
                    "Preserved attribute changed during verification",
                ));
            }
            hash.update(value);
        }
        attributes.push((name.to_vec(), format!("{:x}", hash.finalize())));
    }
    attributes.sort();
    // ACL_TYPE_EXTENDED is 0x100 in the native SDK's sys/acl.h. ENOENT on an
    // already-open descriptor means no extended ACL, not a missing source.
    let acl = unsafe { acl_get_fd_np(fd, 0x100) };
    let acl_text = if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(io(error));
        }
        Vec::new()
    } else {
        let acl = AclAllocation(acl);
        let mut length = 0;
        let text = unsafe { acl_to_text(acl.0, &mut length) };
        if text.is_null() {
            return Err(io(std::io::Error::last_os_error()));
        }
        let text = AclAllocation(text.cast());
        let length = usize::try_from(length).map_err(corrupt)?;
        // SAFETY: acl_to_text returns a live allocation of the reported length.
        unsafe { std::slice::from_raw_parts(text.0.cast::<u8>(), length) }.to_vec()
    };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes the struct on success, checked before use.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    if Witness::of(&file.metadata().map_err(io)?)? != before {
        return Err(issue(
            IssueCode::Conflict,
            "Preserved metadata changed while being inspected",
        ));
    }
    parent.verify().map_err(io)?;
    verification::hash_value(&(
        before.mode,
        before.owner,
        before.created,
        if removing && kind == CloseoutEntryKind::Directory {
            None
        } else {
            Some(before.modified)
        },
        stat.st_flags,
        attributes,
        acl_text,
    ))
}

#[cfg(not(target_os = "macos"))]
fn fingerprint_impl(_: &Path, _: CloseoutEntryKind, _: bool) -> Result<String> {
    Err(issue(
        IssueCode::UnsupportedCapability,
        "Native metadata verification is unavailable",
    ))
}
