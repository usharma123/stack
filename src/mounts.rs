//! Which mount a file is on, as the kernel says for an open descriptor or a name in one.
//!
//! A filesystem's device number tells two filesystems apart, but not a bind mount from the
//! directory it is mounted on when both are of the same filesystem (Linux): only the kernel's id
//! of the mount does. Linux gives it through `statx` (5.8; `STATX_MNT_ID_UNIQUE`, an id never
//! reused within a boot, from 6.8), else through `/proc/self/fdinfo` (3.15). Other platforms
//! have no bind mounts: every mount there has its own device, and no id is needed (`None`).

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

/// Whether mounts on this platform can share a device (Linux bind mounts), so that only a mount
/// id tells them apart.
pub const BINDS: bool = cfg!(target_os = "linux");

/// The kernel's ids of the mount an object is on: the reusable one, and the one never reused
/// within a boot where the kernel has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ids {
    pub id: u64,
    pub unique: Option<u64>,
}

/// The mount the open `fd` is on: `Ok(None)` where mounts have no ids ([`BINDS`] false), an
/// error where they do and none can be read.
pub fn of(fd: &OwnedFd) -> io::Result<Option<Ids>> {
    imp::ids(fd.as_raw_fd(), c"")
}

/// The mount the entry `name` in the directory `dir` is on, never following a link: a link is
/// on the mount of the directory holding it, and a file mounted over (a bind mount of one file)
/// is on its own.
pub fn at(dir: &OwnedFd, name: &CStr) -> io::Result<Option<Ids>> {
    imp::ids(dir.as_raw_fd(), name)
}

/// The mount at the absolute `path`, not following a link in its last component.
pub fn of_path(path: &std::path::Path) -> io::Result<Option<Ids>> {
    let path = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))?;
    imp::ids(libc::AT_FDCWD, &path)
}

/// This process's mount namespace (Linux: the namespace `/proc/self/ns/mnt` names), in which
/// alone a mount id read here means the mount it names; `None` elsewhere or when unreadable.
pub fn namespace() -> Option<u64> {
    imp::namespace()
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Ids;
    use std::ffi::CStr;
    use std::io;

    const STATX_MNT_ID: u32 = 0x1000;
    const STATX_MNT_ID_UNIQUE: u32 = 0x4000;

    /// `statx`'s mount id for `name` in `dir` (the descriptor itself when `name` is empty), asking
    /// for `mask`: the kernel's `struct statx` (256 bytes, `stx_mask` at 0 and `stx_mnt_id` at
    /// 144), called directly, since not every libc Stack builds against declares it. `None` when
    /// the call fails or the kernel does not answer that mask.
    fn statx(dir: libc::c_int, name: &CStr, mask: u32) -> io::Result<Option<u64>> {
        #[repr(C, align(8))]
        struct Statx([u8; 256]);
        let mut buf = Statx([0; 256]);
        let flags = libc::AT_SYMLINK_NOFOLLOW | if name.is_empty() { libc::AT_EMPTY_PATH } else { 0 };
        // SAFETY: statx with a valid descriptor (or AT_FDCWD) and a NUL-terminated name writes at
        // most the 256 bytes of `struct statx` into `buf`.
        let rc = unsafe { libc::syscall(libc::SYS_statx, dir, name.as_ptr(), flags, mask, buf.0.as_mut_ptr()) };
        if rc != 0 {
            let e = io::Error::last_os_error();
            // No statx (before 5.8, or filtered): the caller falls back.
            return if e.raw_os_error() == Some(libc::ENOSYS) || e.raw_os_error() == Some(libc::EPERM) { Ok(None) } else { Err(e) };
        }
        let answered = u32::from_ne_bytes(buf.0[0..4].try_into().expect("4 bytes"));
        Ok((answered & mask != 0).then(|| u64::from_ne_bytes(buf.0[144..152].try_into().expect("8 bytes"))))
    }

    /// `mnt_id` of an open descriptor in `/proc/self/fdinfo`.
    fn fdinfo(fd: libc::c_int) -> Option<u64> {
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).ok()?;
        info.lines().find_map(|l| l.strip_prefix("mnt_id:")).and_then(|v| v.trim().parse().ok())
    }

    pub fn ids(dir: libc::c_int, name: &CStr) -> io::Result<Option<Ids>> {
        let unique = statx(dir, name, STATX_MNT_ID_UNIQUE)?;
        let id = match statx(dir, name, STATX_MNT_ID)? {
            Some(id) => Some(id),
            None if name.is_empty() => fdinfo(dir),
            None => {
                // SAFETY: openat with a valid descriptor (or AT_FDCWD) and a NUL-terminated name;
                // O_PATH opens the entry itself (a link included) without reading it.
                let fd = unsafe { libc::openat(dir, name.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                let id = fdinfo(fd);
                // SAFETY: a descriptor openat just returned, owned here.
                unsafe { libc::close(fd) };
                id
            }
        };
        match id {
            Some(id) => Ok(Some(Ids { id, unique })),
            None => Err(io::Error::other("which mount it is on cannot be told (neither statx nor /proc/self/fdinfo says)")),
        }
    }

    pub fn namespace() -> Option<u64> {
        let link = std::fs::read_link("/proc/self/ns/mnt").ok()?;
        link.to_str()?.strip_prefix("mnt:[")?.strip_suffix(']')?.parse().ok()
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Ids;
    use std::ffi::CStr;
    use std::io;

    pub fn ids(_: libc::c_int, _: &CStr) -> io::Result<Option<Ids>> {
        Ok(None)
    }

    pub fn namespace() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two directories of one filesystem in one temporary directory are on one mount; a missing
    /// entry is an error, not an answer.
    #[test]
    fn entries_of_one_directory_are_on_its_mount() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        std::fs::write(tmp.path().join("f"), b"").unwrap();
        std::os::unix::fs::symlink("/nonexistent", tmp.path().join("l")).unwrap();
        let dir: OwnedFd = std::fs::File::open(tmp.path()).unwrap().into();
        let own = of(&dir).unwrap();
        for name in [c"a", c"f", c"l"] {
            assert_eq!(at(&dir, name).unwrap(), own, "{name:?}");
        }
        assert_eq!(of_path(&tmp.path().join("l")).unwrap(), own);
        if BINDS {
            assert!(own.is_some(), "Linux tells mounts apart");
            assert!(at(&dir, c"missing").is_err());
            assert!(namespace().is_some());
        } else {
            assert_eq!(own, None);
            assert_eq!(namespace(), None);
        }
    }
}
