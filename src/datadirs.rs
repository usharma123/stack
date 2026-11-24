//! Machine-wide record of the data directories this machine's checkouts' services used.
//!
//! mise keeps a service's data under its own state directory, keyed by the project's path, and
//! nothing removes it when a checkout is deleted. `up` records each service's data directory
//! here with the checkout's token (see [`crate::checkout`]), which it also writes beside the
//! directory as its marker. `stack gc` reports the data of checkouts that are gone, and removes
//! it only when asked (`--reclaim-data`) and only while the directory still carries the marker
//! of the checkout recorded here and the supervisor recorded with it confirms the service is
//! not running, so data another checkout took over, data a service may still use, or data
//! stack never recorded, is never touched.
//!
//! A path names whatever is there when it is looked up: a directory above the data replaced by
//! a link, or a copy put in its place marker and all, would make it name other data. So `up`
//! also records where the directory resolved (`location`, with links such as macOS's `/var`
//! resolved) and, once it exists, which directory it is (`identity`, device and inode), and the
//! data is only ever reached through `location` with no link followed, as that very directory
//! (see [`hold`]). It is removed through the directory handles that check opened, never by
//! looking its path up again.

use crate::error::Result;
use crate::state::{read_json, write_json, FileLock};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// One service's data directory, as `up` found it for a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub project: PathBuf,
    pub service: String,
    pub dir: PathBuf,
    /// The checkout's token, as written into the directory's marker.
    pub checkout: String,
    /// The supervisor that runs the service, to confirm it stopped before its data goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<Supervisor>,
    /// `dir` with every link resolved, as `up` found it (see [`locate`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<PathBuf>,
    /// The directory at `location` (device, inode), once `up` saw it exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<[u64; 2]>,
    /// Where the checkout (`project`) was, so its data is reclaimed only once the checkout is
    /// shown to be gone (see [`crate::checkout::Recorded`]); absent in entries an earlier Stack
    /// recorded.
    #[serde(flatten)]
    pub project_at: crate::checkout::Recorded,
}

impl Entry {
    /// Its checkout is shown to be gone (see [`crate::checkout::gone`]).
    pub fn checkout_gone(&self) -> bool {
        crate::checkout::gone(&self.project, &self.project_at)
    }
}

/// A service's supervisor identity: Stack's supervisor directory and the service's name, or,
/// for data an older Stack recorded, the Pitchfork binary, its state directory and the daemon id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supervisor {
    /// Empty for Stack's own supervisor (older releases require the field).
    #[serde(default, with = "crate::state::required_path")]
    pub pitchfork: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub id: String,
}

fn paths(state: &Path) -> (PathBuf, PathBuf) {
    (state.join("data.json"), state.join("data.lock"))
}

/// Record (or replace) where `entry`'s service keeps its data.
pub fn record(state: &Path, entry: Entry) -> Result<()> {
    record_all(state, vec![entry])
}

/// [`record`] for several entries, with one write of the ledger (shared by every checkout, so
/// many checkouts starting at once each hold its lock, and wait for the disk, once).
pub fn record_all(state: &Path, entries: Vec<Entry>) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let mut changed = false;
    for entry in entries {
        if ledger.entries.contains(&entry) {
            continue;
        }
        ledger.entries.retain(|e| !(e.project == entry.project && e.service == entry.service) && e.dir != entry.dir);
        ledger.entries.push(entry);
        changed = true;
    }
    if changed {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// Every recorded entry.
pub fn entries(state: &Path) -> Result<Vec<Entry>> {
    Ok(read_json::<Ledger>(&paths(state).0)?.entries)
}

/// Record where each entry's checkout is now, for those still there (see
/// [`crate::checkout::Refresh`]), under the ledger's lock; written only when one changed.
pub fn refresh_checkouts(state: &Path) -> Result<()> {
    let (file, lock) = paths(state);
    if !file.exists() {
        return Ok(());
    }
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let mut refresh = crate::checkout::Refresh::default();
    let mut changed = false;
    for entry in &mut ledger.entries {
        changed |= refresh.apply(&entry.project, &mut entry.project_at);
    }
    if changed {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// Drop the entries `gone` selects, under the ledger's lock.
pub fn forget(state: &Path, gone: impl Fn(&Entry) -> bool) -> Result<()> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let before = ledger.entries.len();
    ledger.entries.retain(|e| !gone(e));
    if ledger.entries.len() != before {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// Where `dir` is with every link resolved, and which directory it is (device, inode) when it
/// exists. Components not created yet are kept as given below the deepest one that exists.
pub fn locate(dir: &Path) -> (Option<PathBuf>, Option<[u64; 2]>) {
    if !dir.is_absolute() {
        return (None, None);
    }
    let mut rest = Vec::new();
    let mut base = dir;
    let resolved = loop {
        if let Ok(resolved) = std::fs::canonicalize(base) {
            break resolved;
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return (None, None),
        }
    };
    let location = rest.iter().rev().fold(resolved, |path, name| path.join(name));
    (Some(location.clone()), identity(&location))
}

#[cfg(unix)]
fn identity(path: &Path) -> Option<[u64; 2]> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().filter(|m| m.is_dir()).map(|m| [m.dev(), m.ino()])
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<[u64; 2]> {
    None
}

/// Bind the entry recording `dir` for `checkout` to where `dir` now resolves and which
/// directory it is, now that it exists: `up` calls this once the service using it verified.
pub fn bind(state: &Path, dir: &Path, checkout: &str) -> Result<()> {
    bind_all(state, &[dir], checkout)
}

/// [`bind`] for several directories of one checkout, with at most one write of the ledger.
/// Nothing is locked or written when every entry is already bound as found.
pub fn bind_all(state: &Path, dirs: &[&Path], checkout: &str) -> Result<()> {
    let found: Vec<(&Path, Option<PathBuf>, [u64; 2])> = dirs
        .iter()
        .filter_map(|dir| {
            let (location, identity) = locate(dir);
            identity.map(|identity| (*dir, location, identity))
        })
        .collect();
    let bound = |ledger: &Ledger| {
        found.iter().all(|(dir, location, identity)| {
            ledger.entries.iter().filter(|e| e.dir == *dir && e.checkout == checkout).all(|e| (e.location.as_ref(), e.identity) == (location.as_ref(), Some(*identity)))
        })
    };
    // Read without the lock first: bound already is the common case (every later `up`).
    if found.is_empty() || read_json::<Ledger>(&paths(state).0).is_ok_and(|l| bound(&l)) {
        return Ok(());
    }
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let mut changed = false;
    for (dir, location, identity) in &found {
        for entry in ledger.entries.iter_mut().filter(|e| e.dir == *dir && e.checkout == checkout) {
            if (entry.location.as_ref(), entry.identity) != (location.as_ref(), Some(*identity)) {
                (entry.location, entry.identity) = (location.clone(), Some(*identity));
                changed = true;
            }
        }
    }
    if changed {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// A data directory a restore replaced, to bind its record to the replacement (see
/// [`rebind`]).
#[derive(Debug, Clone)]
pub struct Rebind<'a> {
    pub project: &'a Path,
    pub service: &'a str,
    pub dir: &'a Path,
    /// Where it resolved when it was replaced.
    pub location: &'a Path,
    /// The directory replaced (`None`: nothing was there).
    pub from: Option<[u64; 2]>,
    /// The directory that replaced it.
    pub to: [u64; 2],
}

impl Rebind<'_> {
    /// `entry` is this checkout's record of the directory, still as the restore found it (or
    /// already bound to the replacement): the same checkout, service and path, resolving to the
    /// same place, naming the directory replaced or its replacement. A record that names no
    /// directory yet (nothing was there) only has to resolve to the same place.
    pub fn matches(&self, entry: &Entry, checkout: &str) -> bool {
        entry.project == self.project
            && entry.service == self.service
            && entry.dir == self.dir
            && entry.checkout == checkout
            && entry.location.as_deref().map_or(self.from.is_none(), |l| l == self.location)
            && (entry.identity == Some(self.to) || self.from.map_or(true, |from| entry.identity == Some(from)))
    }
}

/// Bind each record of a restore's directories to the directory that replaced it, by the
/// identity the restore journaled, never by what the path names now: all at once, and only
/// when every record still [`Rebind::matches`]; otherwise nothing is written.
pub fn rebind(state: &Path, checkout: &str, rebinds: &[Rebind]) -> Result<()> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let mut changed = false;
    for r in rebinds {
        let Some(entry) = ledger.entries.iter_mut().find(|e| r.matches(e, checkout)) else {
            return Err(crate::error::StackError::new(
                "state_invalid",
                format!("Stack's record no longer names {} as {}'s data directory for this checkout", r.dir.display(), r.service),
            ));
        };
        if (entry.location.as_deref(), entry.identity) != (Some(r.location), Some(r.to)) {
            (entry.location, entry.identity) = (Some(r.location.to_path_buf()), Some(r.to));
            changed = true;
        }
    }
    if changed {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// What [`hold`] found for an entry.
pub enum Found {
    /// The recorded directory, open, with its parent.
    Held(Held),
    /// Nothing is at the recorded location (or above it) any more.
    Gone,
    /// Something else is there: what, said for a person. Never touched.
    Other(String),
    /// The entry cannot say which directory is its own: why.
    Unbound(String),
}

/// Open the directory `entry` records, as `up` saw it: by its recorded `location`, from the
/// root down with no link followed (a link anywhere, even one to the same place, is refused),
/// and only if it is the recorded directory (device and inode), so neither a directory above
/// it replaced by a link nor a copy put in its place, marker and all, is taken for it.
pub fn hold(entry: &Entry) -> Found {
    let Some(location) = &entry.location else {
        return Found::Unbound("it was recorded before Stack noted which directory it is".into());
    };
    let Some(identity) = entry.identity else {
        return Found::Unbound("Stack never saw this directory while its service ran, so it cannot tell it is the same one".into());
    };
    imp::hold(location, identity)
}

#[cfg(unix)]
pub use imp::{exchange, remove_tree, Held, Swapped};
#[cfg(all(unix, test))]
pub(crate) use imp::NO_EXCHANGE;
/// How data removal learns which mount a directory is on, for tests to stand in for the kernel.
#[cfg(all(unix, test))]
pub(crate) mod imp_boundary {
    pub(crate) use super::imp::{Boundary, Kernel, Mount};
}

#[cfg(unix)]
mod imp {
    use super::{Component, Found, Path};
    use std::ffi::{CStr, CString, OsStr};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    /// A recorded data directory, open, and the directory holding it and its marker. Whatever
    /// happens to their paths afterwards, these handles stay on the directories checked.
    pub struct Held {
        parent: OwnedFd,
        name: CString,
        dir: OwnedFd,
        identity: [u64; 2],
    }

    fn cstring(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))
    }

    fn stat_at(dir: &OwnedFd, name: &CStr) -> io::Result<libc::stat> {
        // SAFETY: fstatat with a valid descriptor, a NUL-terminated name and local storage.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            match libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) {
                0 => Ok(st),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    fn fstat(fd: &OwnedFd) -> io::Result<libc::stat> {
        // SAFETY: fstat with a valid descriptor and local storage.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            match libc::fstat(fd.as_raw_fd(), &mut st) {
                0 => Ok(st),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    fn is(st: &libc::stat, kind: libc::mode_t) -> bool {
        st.st_mode & libc::S_IFMT == kind
    }

    #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
    fn id(st: &libc::stat) -> [u64; 2] {
        [st.st_dev as u64, st.st_ino as u64]
    }

    /// Open `name` in `dir` (the root when `None`), never following a link.
    fn open_at(dir: Option<&OwnedFd>, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        let at = dir.map_or(libc::AT_FDCWD, |d| d.as_raw_fd());
        // SAFETY: openat with a valid descriptor (or AT_FDCWD with an absolute name) and a
        // NUL-terminated name; the descriptor returned is owned here.
        let fd = unsafe { libc::openat(at, name.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a descriptor openat just returned, owned by nothing else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Open the directory `name` in `dir`: `Ok(None)` when nothing is there, an error saying
    /// what is there instead of a directory.
    fn open_dir(dir: &OwnedFd, name: &CStr, shown: &Path) -> std::result::Result<Option<OwnedFd>, String> {
        let st = match stat_at(dir, name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{} cannot be checked ({e})", shown.display())),
            Ok(st) => st,
        };
        if is(&st, libc::S_IFLNK) {
            return Err(format!("{} is now a link", shown.display()));
        }
        if !is(&st, libc::S_IFDIR) {
            return Err(format!("{} is no longer a directory", shown.display()));
        }
        // O_NOFOLLOW: a link put there since the check fails here rather than being followed.
        open_at(Some(dir), name, libc::O_RDONLY | libc::O_DIRECTORY)
            .map(Some)
            .map_err(|e| format!("{} changed while it was checked ({e})", shown.display()))
    }

    /// The directory at `location` (an absolute path with nothing to resolve) and the directory
    /// holding it, opened from the root down one component at a time through the descriptor of
    /// the one above, never following a link (a link anywhere, even to the same place, is
    /// refused): `Ok(None)` when nothing is there, an error saying what is there instead.
    fn open_path(location: &Path) -> std::result::Result<Option<(OwnedFd, CString, OwnedFd)>, String> {
        let (Some(parent_path), Some(name)) = (location.parent(), location.file_name()) else {
            return Err(format!("{} names no directory", location.display()));
        };
        if !location.is_absolute() {
            return Err(format!("{} is not a resolved path", location.display()));
        }
        let mut parent = open_at(None, c"/", libc::O_RDONLY | libc::O_DIRECTORY).map_err(|e| format!("cannot open / ({e})"))?;
        let mut walked = std::path::PathBuf::from("/");
        for component in parent_path.components() {
            let part = match component {
                Component::RootDir => continue,
                Component::Normal(part) => part,
                _ => return Err(format!("{} is not a resolved path", location.display())),
            };
            walked.push(part);
            let part = cstring(part).map_err(|_| format!("{} cannot be named", walked.display()))?;
            match open_dir(&parent, &part, &walked)? {
                Some(fd) => parent = fd,
                None => return Ok(None),
            }
        }
        let name = cstring(name).map_err(|_| format!("{} cannot be named", location.display()))?;
        Ok(open_dir(&parent, &name, location)?.map(|dir| (parent, name, dir)))
    }

    pub fn hold(location: &Path, identity: [u64; 2]) -> Found {
        let (parent, name, dir) = match open_path(location) {
            Ok(Some(held)) => held,
            Ok(None) => return Found::Gone,
            Err(why) => return Found::Other(why),
        };
        match fstat(&dir) {
            Ok(st) if id(&st) == identity => Found::Held(Held { parent, name, dir, identity }),
            Ok(_) => Found::Other(format!("{} is not the directory Stack recorded (it was replaced or copied there)", location.display())),
            Err(e) => Found::Other(format!("{} cannot be checked ({e})", location.display())),
        }
    }

    impl Held {
        /// The contents of `suffix`'s file beside the directory (`<name><suffix>`), read
        /// through the held parent without following a link; `None` when unreadable.
        pub fn beside(&self, suffix: &str) -> Option<Vec<u8>> {
            let mut name = self.name.as_bytes().to_vec();
            name.extend_from_slice(suffix.as_bytes());
            let name = CString::new(name).ok()?;
            let fd = open_at(Some(&self.parent), &name, libc::O_RDONLY).ok()?;
            let mut bytes = Vec::new();
            io::Read::read_to_end(&mut std::fs::File::from(fd), &mut bytes).ok()?;
            Some(bytes)
        }

        /// Bytes of regular files in the directory, never following links, nor counting what a
        /// mount inside it holds.
        pub fn size(&self) -> u64 {
            self.size_within(&Kernel)
        }

        /// [`Held::size`], learning which mount each directory is on from `boundary`.
        pub(crate) fn size_within(&self, boundary: &dyn Boundary) -> u64 {
            let Ok(st) = fstat(&self.dir) else { return 0 };
            let Ok(mount) = boundary.mount(&self.dir, &st) else { return 0 };
            walk(&self.dir, Walk::Size, &Within { mount, boundary }, Path::new("")).unwrap_or(0)
        }

        /// Remove everything in the held directory, then the directory and the `suffix` file
        /// beside it from the held parent, provided the name there still is this directory.
        /// Returns the bytes of regular files removed.
        ///
        /// Nothing on another mount is ever removed: the directory must be on its parent's mount
        /// (it is no mount point itself), and nothing under it may be a mount point, a bind mount
        /// of the same filesystem included, whether a directory or a single file mounted over.
        /// Both are checked on every directory as opened and every other entry as named in the
        /// directory held open, not as a path names it, before anything is removed and again
        /// while removing; a mount found before refuses the removal whole, one that appears
        /// during it stops it there. A directory that cannot be listed whole, or an entry that
        /// cannot be checked, refuses the removal before anything is removed: nothing is removed
        /// after a check that did not finish. A link is removed itself, never followed.
        pub fn remove(self, suffix: &str) -> io::Result<u64> {
            self.remove_within(Some(suffix), &Kernel)
        }

        /// [`Held::remove`], learning which mount each directory is on from `boundary`, and
        /// removing the `suffix` file beside it when there is one.
        pub(crate) fn remove_within(self, suffix: Option<&str>, boundary: &dyn Boundary) -> io::Result<u64> {
            let mount = boundary.mount(&self.dir, &fstat(&self.dir)?)?;
            let parent = boundary.mount(&self.parent, &fstat(&self.parent)?)?;
            if mount != parent {
                return Err(io::Error::other(format!(
                    "it is a mount point ({}); Stack never removes what a mount holds, and nothing was removed",
                    mount.differs(&parent)
                )));
            }
            let within = Within { mount, boundary };
            walk(&self.dir, Walk::Check, &within, Path::new(""))?;
            let bytes = walk(&self.dir, Walk::Remove, &within, Path::new(""))?;
            let now = stat_at(&self.parent, &self.name)?;
            if id(&now) != self.identity {
                return Err(io::Error::other("the directory was replaced while it was being removed; what replaced it is left alone"));
            }
            // SAFETY: unlinkat with the held parent and a NUL-terminated name.
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let Some(suffix) = suffix else { return Ok(bytes) };
            let mut marker = self.name.as_bytes().to_vec();
            marker.extend_from_slice(suffix.as_bytes());
            let marker = CString::new(marker).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))?;
            // SAFETY: as above.
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), marker.as_ptr(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
            Ok(bytes)
        }
    }

    /// Remove the directory at `path` (an absolute path with nothing to resolve), provided it is
    /// the directory `identity` (device, inode), and everything under it as [`Held::remove`]
    /// does: reached from the root down through held descriptors with no link followed (a link
    /// anywhere on the way refuses it), never following a link inside, and never removing
    /// anything when it or anything under it is on another mount than the directory holding it.
    /// A path that names another directory now (a stale record of one) is left alone.
    pub fn remove_tree(path: &Path, identity: [u64; 2]) -> io::Result<u64> {
        remove_tree_within(path, identity, &Kernel)
    }

    pub(crate) fn remove_tree_within(path: &Path, identity: [u64; 2], boundary: &dyn Boundary) -> io::Result<u64> {
        let (parent, name, dir) = match open_path(path) {
            Ok(Some(held)) => held,
            Ok(None) => return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not there", path.display()))),
            Err(why) => return Err(io::Error::other(why)),
        };
        if id(&fstat(&dir)?) != identity {
            return Err(io::Error::other(format!("{} is not the directory to remove (it was replaced); left alone", path.display())));
        }
        Held { parent, name, dir, identity }.remove_within(None, boundary)
    }

    impl Held {
        /// Rename the held directory to `to`, an absolute path where nothing is (or an empty
        /// directory), and remove the `suffix` file beside its old name, provided the name in
        /// the held parent still is this directory; it is then confirmed to be the directory
        /// now at `to`, and renamed back otherwise. Its contents are never copied: a `to` on
        /// another file system fails, and nothing moves.
        pub fn move_to(self, to: &Path, suffix: &str) -> io::Result<()> {
            let now = stat_at(&self.parent, &self.name)?;
            if id(&now) != self.identity {
                return Err(io::Error::other("the directory was replaced since it was checked; what replaced it is left alone"));
            }
            let target = cstring(to.as_os_str())?;
            // SAFETY: renameat with the held parent, NUL-terminated names and AT_FDCWD for an
            // absolute target.
            if unsafe { libc::renameat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_FDCWD, target.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let moved = std::fs::symlink_metadata(to).ok().map(|m| {
                use std::os::unix::fs::MetadataExt;
                [m.dev(), m.ino()]
            });
            if moved != Some(self.identity) {
                // SAFETY: as above, the other way.
                unsafe { libc::renameat(libc::AT_FDCWD, target.as_ptr(), self.parent.as_raw_fd(), self.name.as_ptr()) };
                return Err(io::Error::other(format!("{} changed while the data was moved there; the data was moved back", to.display())));
            }
            let mut marker = self.name.as_bytes().to_vec();
            marker.extend_from_slice(suffix.as_bytes());
            let marker = CString::new(marker).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))?;
            // SAFETY: unlinkat with the held parent and a NUL-terminated name.
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), marker.as_ptr(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    thread_local! {
        /// Tests take the two-rename path of platforms and filesystems without an exchange.
        pub(crate) static NO_EXCHANGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Exchange `a` in `dir_a` and `b` in `dir_b` (`None`: resolved from the working directory)
    /// in one step: `renameatx_np(RENAME_SWAP)` on macOS, `renameat2(RENAME_EXCHANGE)` on Linux
    /// (by syscall number: musl declares no wrapper). `Ok(false)` when the platform or the
    /// filesystem has no exchange, so the caller falls back.
    fn exchange_at(dir_a: Option<&OwnedFd>, a: &CStr, dir_b: Option<&OwnedFd>, b: &CStr) -> io::Result<bool> {
        #[cfg(test)]
        if NO_EXCHANGE.with(std::cell::Cell::get) {
            return Ok(false);
        }
        let (fa, fb) = (dir_a.map_or(libc::AT_FDCWD, |d| d.as_raw_fd()), dir_b.map_or(libc::AT_FDCWD, |d| d.as_raw_fd()));
        #[cfg(target_os = "macos")]
        // SAFETY: valid descriptors (or AT_FDCWD) and NUL-terminated names, only read.
        let rc = unsafe { libc::renameatx_np(fa, a.as_ptr(), fb, b.as_ptr(), libc::RENAME_SWAP) };
        #[cfg(target_os = "linux")]
        // SAFETY: as above; renameat2 only reads its arguments.
        let rc = unsafe { libc::syscall(libc::SYS_renameat2, fa, a.as_ptr(), fb, b.as_ptr(), libc::RENAME_EXCHANGE) };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let rc = {
            let _ = (fa, fb, a, b);
            return Ok(false);
        };
        if rc == 0 {
            return Ok(true);
        }
        let e = io::Error::last_os_error();
        // No such call (an old kernel, a seccomp filter), or a filesystem without exchange
        // (EOPNOTSUPP is ENOTSUP on Linux, another number on macOS).
        let code = e.raw_os_error().unwrap_or(0);
        if [libc::ENOSYS, libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP].contains(&code) {
            return Ok(false);
        }
        Err(e)
    }

    /// [`exchange_at`] of two absolute paths.
    pub fn exchange(a: &Path, b: &Path) -> io::Result<bool> {
        exchange_at(None, &cstring(a.as_os_str())?, None, &cstring(b.as_os_str())?)
    }

    /// How [`Held::swap_in`] put a directory in the held directory's place.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Swapped {
        /// In one step: the held directory now has the other's name.
        Exchanged,
        /// Two renames: the held directory was renamed aside first.
        Renamed,
    }

    impl Held {
        /// Which directory this is (device, inode).
        pub fn identity(&self) -> [u64; 2] {
            self.identity
        }

        /// The directory named `name` beside the held one (device, inode), never following a
        /// link; `None` when nothing, or no directory, is there.
        pub fn sibling(&self, name: &OsStr) -> Option<[u64; 2]> {
            let st = stat_at(&self.parent, &cstring(name).ok()?).ok()?;
            is(&st, libc::S_IFDIR).then(|| id(&st))
        }

        /// Whether the held directory is on its parent's mount (no mount point itself), as
        /// [`Held::remove`] requires before it removes anything.
        pub fn on_parent_mount(&self) -> io::Result<bool> {
            Ok(Kernel.mount(&self.dir, &fstat(&self.dir)?)? == Kernel.mount(&self.parent, &fstat(&self.parent)?)?)
        }

        /// Put the directory `fresh` (a name beside the held one) in the held directory's place,
        /// through the held parent, provided the name still is the held directory: one exchange
        /// where the filesystem has it (the held directory is then at `fresh`), else the held
        /// directory is renamed to `aside` and `fresh` to its name (and back when that fails).
        /// The held directory is then confirmed to be where it went, and everything put back
        /// otherwise.
        pub fn swap_in(&self, fresh: &OsStr, aside: &OsStr) -> io::Result<Swapped> {
            let (fresh, aside) = (cstring(fresh)?, cstring(aside)?);
            if id(&stat_at(&self.parent, &self.name)?) != self.identity {
                return Err(io::Error::other("the data directory was replaced since it was checked; nothing was swapped"));
            }
            let renameat = |from: &CStr, to: &CStr| {
                // SAFETY: renameat within the held parent, with NUL-terminated names.
                match unsafe { libc::renameat(self.parent.as_raw_fd(), from.as_ptr(), self.parent.as_raw_fd(), to.as_ptr()) } {
                    0 => Ok(()),
                    _ => Err(io::Error::last_os_error()),
                }
            };
            let at = |name: &CStr| stat_at(&self.parent, name).ok().map(|st| id(&st));
            if exchange_at(Some(&self.parent), &fresh, Some(&self.parent), &self.name)? {
                if at(&fresh) != Some(self.identity) {
                    let _ = exchange_at(Some(&self.parent), &fresh, Some(&self.parent), &self.name);
                    return Err(io::Error::other("the data directory changed while it was swapped; it was swapped back"));
                }
                return Ok(Swapped::Exchanged);
            }
            renameat(&self.name, &aside)?;
            if at(&aside) != Some(self.identity) {
                let _ = renameat(&aside, &self.name);
                return Err(io::Error::other("the data directory changed while it was set aside; it was put back"));
            }
            if let Err(e) = renameat(&fresh, &self.name) {
                let _ = renameat(&aside, &self.name);
                return Err(e);
            }
            Ok(Swapped::Renamed)
        }
    }

    /// The names in `dir`, without `.` and `..`.
    fn names(dir: &OwnedFd) -> io::Result<Vec<CString>> {
        // SAFETY: dup of a valid descriptor; fdopendir takes the copy, closedir closes it.
        unsafe {
            let copy = libc::dup(dir.as_raw_fd());
            if copy < 0 {
                return Err(io::Error::last_os_error());
            }
            let stream = libc::fdopendir(copy);
            if stream.is_null() {
                let e = io::Error::last_os_error();
                libc::close(copy);
                return Err(e);
            }
            libc::rewinddir(stream);
            let mut names = Vec::new();
            loop {
                // readdir answers NULL both at the end and on an error: only errno tells them
                // apart, so it is cleared first. A listing cut short is never taken for whole.
                set_errno(0);
                let entry = libc::readdir(stream);
                if entry.is_null() {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error().is_some_and(|code| code != 0) {
                        libc::closedir(stream);
                        return Err(io::Error::new(e.kind(), format!("the directory cannot be listed whole ({e})")));
                    }
                    break;
                }
                let name = CStr::from_ptr((*entry).d_name.as_ptr());
                if name != c"." && name != c".." {
                    names.push(name.to_owned());
                }
            }
            libc::closedir(stream);
            Ok(names)
        }
    }

    fn set_errno(value: libc::c_int) {
        // SAFETY: the calling thread's errno, which it alone writes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                *libc::__errno_location() = value;
            }
            #[cfg(target_os = "macos")]
            {
                *libc::__error() = value;
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let _ = value;
        }
    }

    /// Which mount a directory is on: its filesystem's device and, on Linux, the kernel's id of
    /// the mount, which tells a bind mount of the same filesystem (same device) from the
    /// directory it is mounted on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Mount {
        pub dev: u64,
        pub id: Option<u64>,
    }

    impl Mount {
        /// How `self` differs from `other`, said for a person.
        fn differs(&self, other: &Mount) -> String {
            if self.dev != other.dev {
                format!("device {}, not {}", self.dev, other.dev)
            } else {
                format!("mount {}, not {}, of the same filesystem", self.id.unwrap_or(0), other.id.unwrap_or(0))
            }
        }
    }

    /// How the mount of an open directory, or of an entry in one, is learned, and how a
    /// directory is listed: from the kernel, or as a test says.
    pub(crate) trait Boundary {
        /// The mount `dir` (whose `fstat` is `st`) is on; an error when it cannot be told.
        fn mount(&self, dir: &OwnedFd, st: &libc::stat) -> io::Result<Mount>;
        /// The mount the entry `name` in `dir` (whose `fstatat`, not following a link, is `st`)
        /// is on: a file mounted over is on its own; an error when it cannot be told.
        fn entry_mount(&self, dir: &OwnedFd, name: &CStr, st: &libc::stat) -> io::Result<Mount>;
        /// The names in `dir`, all of them, or an error.
        fn list(&self, dir: &OwnedFd) -> io::Result<Vec<CString>> {
            names(dir)
        }
    }

    /// The kernel's answer, from the open directory itself, or the entry named in it (so a mount
    /// put at its path after it was opened changes nothing). Linux: `statx`'s mount id, else
    /// `/proc/self/fdinfo` (see [`crate::mounts`]); neither answering refuses, since without it a
    /// bind mount of the same filesystem cannot be told from a directory. macOS has no bind
    /// mounts: every mount is a filesystem of its own, with its own device.
    pub(crate) struct Kernel;

    impl Boundary for Kernel {
        fn mount(&self, dir: &OwnedFd, st: &libc::stat) -> io::Result<Mount> {
            Ok(Mount { dev: id(st)[0], id: crate::mounts::of(dir)?.map(|m| m.id) })
        }

        fn entry_mount(&self, dir: &OwnedFd, name: &CStr, st: &libc::stat) -> io::Result<Mount> {
            Ok(Mount { dev: id(st)[0], id: crate::mounts::at(dir, name)?.map(|m| m.id) })
        }
    }

    /// The mount a walk keeps to, and how it learns where each directory is.
    struct Within<'a> {
        mount: Mount,
        boundary: &'a dyn Boundary,
    }

    /// What a [`walk`] does: count bytes (a mount inside is not counted), check that nothing
    /// under the directory is on another mount, or remove everything under it (stopping at a
    /// mount).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Walk {
        Size,
        Check,
        Remove,
    }

    /// Bytes of regular files under `dir` (`at` below the directory walked from), removing
    /// everything under it for [`Walk::Remove`]. Links are never followed: a link is removed (or
    /// counted as nothing) itself. Each directory is opened, then checked as opened: still the
    /// directory looked at, and on the walk's mount; every other entry is checked on the walk's
    /// mount as named in the directory held open (a file mounted over is not), just before it
    /// is removed. A directory is removed only while its name still is the directory emptied.
    fn walk(dir: &OwnedFd, mode: Walk, within: &Within, at: &Path) -> io::Result<u64> {
        let mut bytes = 0;
        for name in within.boundary.list(dir)? {
            let st = match stat_at(dir, &name) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                other => other?,
            };
            let directory = is(&st, libc::S_IFDIR);
            let shown = at.join(OsStr::from_bytes(name.as_bytes()));
            let refuse = |mount: &Mount| {
                io::Error::other(format!(
                    "{} inside it is a mount point ({}); Stack never removes what a mount holds{}",
                    shown.display(),
                    mount.differs(&within.mount),
                    if mode == Walk::Check { ", and nothing was removed" } else { "; removal stopped there" }
                ))
            };
            if directory {
                let child = open_at(Some(dir), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
                let opened = fstat(&child)?;
                if id(&opened) != id(&st) {
                    return Err(io::Error::other(format!("{} changed while it was walked; left alone", shown.display())));
                }
                let mount = within.boundary.mount(&child, &opened)?;
                if mount != within.mount {
                    if mode == Walk::Size {
                        continue;
                    }
                    return Err(refuse(&mount));
                }
                bytes += walk(&child, mode, within, &shown)?;
                if mode == Walk::Remove {
                    match stat_at(dir, &name) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Ok(now) if id(&now) == id(&opened) => {}
                        Ok(_) => return Err(io::Error::other(format!("{} was replaced while it was removed; what replaced it is left alone", shown.display()))),
                        Err(e) => return Err(e),
                    }
                }
            } else {
                let mount = match within.boundary.entry_mount(dir, &name, &st) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    other => other?,
                };
                if mount != within.mount {
                    if mode == Walk::Size {
                        continue;
                    }
                    return Err(refuse(&mount));
                }
                if is(&st, libc::S_IFREG) {
                    bytes += u64::try_from(st.st_size).unwrap_or(0);
                }
            }
            if mode == Walk::Remove {
                let flag = if directory { libc::AT_REMOVEDIR } else { 0 };
                // SAFETY: unlinkat with a held descriptor and a NUL-terminated name.
                if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flag) } != 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::NotFound {
                        return Err(e);
                    }
                }
            }
        }
        Ok(bytes)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{Found, Path};

    pub fn hold(_: &Path, _: [u64; 2]) -> Found {
        Found::Unbound("Stack reclaims data only on Unix".into())
    }
}

#[cfg(not(unix))]
pub struct Held;

#[cfg(not(unix))]
pub fn remove_tree(path: &Path) -> std::io::Result<u64> {
    std::fs::remove_dir_all(path).map(|()| 0)
}

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swapped {
    Exchanged,
    Renamed,
}

#[cfg(not(unix))]
pub fn exchange(_: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(not(unix))]
impl Held {
    pub fn identity(&self) -> [u64; 2] {
        [0, 0]
    }
    pub fn sibling(&self, _: &std::ffi::OsStr) -> Option<[u64; 2]> {
        None
    }
    pub fn on_parent_mount(&self) -> std::io::Result<bool> {
        Err(std::io::Error::other("not supported"))
    }
    pub fn swap_in(&self, _: &std::ffi::OsStr, _: &std::ffi::OsStr) -> std::io::Result<Swapped> {
        Err(std::io::Error::other("not supported"))
    }
    pub fn beside(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    pub fn size(&self) -> u64 {
        0
    }
    pub fn remove(self, _: &str) -> std::io::Result<u64> {
        Err(std::io::Error::other("not supported"))
    }
    pub fn move_to(self, _: &Path, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("not supported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(project: &str, service: &str, dir: &str, checkout: &str) -> Entry {
        Entry { project: project.into(), service: service.into(), dir: dir.into(), checkout: checkout.into(), supervisor: None, location: None, identity: None, project_at: Default::default() }
    }

    #[test]
    fn a_service_has_one_entry_and_a_directory_one_owner() {
        let state = tempfile::tempdir().unwrap();
        record(state.path(), entry("/a", "pg", "/d/a", "t1")).unwrap();
        record(state.path(), entry("/a", "pg", "/d/a", "t1")).unwrap();
        record(state.path(), entry("/a", "redis", "/d/ar", "t1")).unwrap();
        // The same checkout path recreated: its new token replaces the old entry.
        record(state.path(), entry("/a", "pg", "/d/a", "t2")).unwrap();
        // Another project took the directory over: the earlier claim on it goes.
        record(state.path(), entry("/b", "pg", "/d/ar", "t3")).unwrap();
        assert_eq!(entries(state.path()).unwrap(), [entry("/a", "pg", "/d/a", "t2"), entry("/b", "pg", "/d/ar", "t3")]);
        forget(state.path(), |e| e.project == Path::new("/a")).unwrap();
        assert_eq!(entries(state.path()).unwrap(), [entry("/b", "pg", "/d/ar", "t3")]);
    }

    /// `parent/data` with a file, its marker beside it, and a file of someone's beside both;
    /// the entry `up` would record for it.
    #[cfg(unix)]
    fn recorded(root: &Path) -> (PathBuf, Entry) {
        let parent = root.join("parent");
        std::fs::create_dir_all(parent.join("data/sub")).unwrap();
        std::fs::write(parent.join("data/rows"), b"123").unwrap();
        std::fs::write(parent.join("data/sub/more"), b"45").unwrap();
        std::fs::write(parent.join("data.stack-checkout"), b"{\"checkout\":\"t1\"}").unwrap();
        std::fs::write(parent.join("unsaved.txt"), b"keep me").unwrap();
        let dir = parent.join("data");
        let (location, identity) = locate(&dir);
        (parent, Entry { location, identity, ..entry("/gone", "pg", dir.to_str().unwrap(), "t1") })
    }

    #[cfg(unix)]
    fn held(found: Found) -> Held {
        match found {
            Found::Held(held) => held,
            Found::Gone => panic!("gone"),
            Found::Other(why) | Found::Unbound(why) => panic!("{why}"),
        }
    }

    #[cfg(unix)]
    fn refused(found: Found) -> String {
        match found {
            Found::Other(why) => why,
            Found::Held(_) => panic!("held"),
            Found::Gone => panic!("gone"),
            Found::Unbound(why) => panic!("unbound: {why}"),
        }
    }

    /// Whether the copy of `recorded` at `root` is all there.
    #[cfg(unix)]
    fn intact(root: &Path) -> bool {
        std::fs::read(root.join("data/rows")).is_ok_and(|b| b == b"123") && root.join("data/sub/more").exists() && root.join("data.stack-checkout").exists()
    }

    #[cfg(unix)]
    fn copy(from: &Path, to: &Path) {
        assert!(std::process::Command::new("cp").arg("-Rp").arg(from).arg(to).status().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn the_recorded_directory_is_reclaimed_through_what_was_checked_and_nothing_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        // macOS's temporary directory is under /var, a link: the location has it resolved.
        assert_eq!(entry.location.as_deref(), Some(parent.join("data").canonicalize().unwrap().as_path()));
        let found = held(hold(&entry));
        assert_eq!(found.beside(".stack-checkout").as_deref(), Some(&b"{\"checkout\":\"t1\"}"[..]));
        assert_eq!(found.size(), 5);
        assert_eq!(found.remove(".stack-checkout").unwrap(), 5);
        assert!(!parent.join("data").exists() && !parent.join("data.stack-checkout").exists());
        assert_eq!(std::fs::read(parent.join("unsaved.txt")).unwrap(), b"keep me");
        assert!(matches!(hold(&entry), Found::Gone));
    }

    #[cfg(unix)]
    #[test]
    fn a_platform_alias_in_the_recorded_path_is_not_a_redirection() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        // Recorded through an alias that was already there, as mise names `/tmp/...` on macOS.
        std::os::unix::fs::symlink(&real, root.path().join("alias")).unwrap();
        let (_, entry) = recorded(&root.path().join("alias"));
        assert!(entry.location.as_ref().unwrap().starts_with(real.canonicalize().unwrap()));
        held(hold(&entry)).remove(".stack-checkout").unwrap();
        assert!(!real.join("parent/data").exists() && real.join("parent/unsaved.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_or_a_copy_in_the_recorded_place_is_never_taken_for_it() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let (original, backup) = (root.path().join("original"), root.path().join("backup"));
        // The data directory itself replaced by a link.
        std::fs::rename(parent.join("data"), root.path().join("data-moved")).unwrap();
        std::os::unix::fs::symlink(root.path().join("data-moved"), parent.join("data")).unwrap();
        assert!(refused(hold(&entry)).contains("is now a link"));
        std::fs::remove_file(parent.join("data")).unwrap();
        std::fs::rename(root.path().join("data-moved"), parent.join("data")).unwrap();
        // A directory above it moved away, and a link to an independent copy, marker and all,
        // put in its place.
        std::fs::rename(&parent, &original).unwrap();
        copy(&original, &backup);
        std::os::unix::fs::symlink(&backup, &parent).unwrap();
        assert!(refused(hold(&entry)).contains("is now a link"));
        // The same link, to the very directory recorded: still a link, still refused.
        std::fs::remove_file(&parent).unwrap();
        std::os::unix::fs::symlink(&original, &parent).unwrap();
        assert!(refused(hold(&entry)).contains("is now a link"));
        std::fs::remove_file(&parent).unwrap();
        // A copy put in its place with no link, marker and all.
        copy(&original, &parent);
        assert!(refused(hold(&entry)).contains("not the directory Stack recorded"));
        assert!(intact(&original) && intact(&backup) && intact(&parent));
        // A directory recorded before it was seen, or before locations were recorded.
        assert!(matches!(hold(&Entry { identity: None, ..entry.clone() }), Found::Unbound(_)));
        assert!(matches!(hold(&Entry { location: None, ..entry }), Found::Unbound(_)));
    }

    #[cfg(unix)]
    #[test]
    fn what_replaces_the_checked_directory_before_its_removal_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let found = held(hold(&entry));
        // Between the check and the removal, its parent is moved and a link to a copy put there.
        let (moved, backup) = (root.path().join("moved"), root.path().join("backup"));
        std::fs::rename(&parent, &moved).unwrap();
        copy(&moved, &backup);
        std::os::unix::fs::symlink(&backup, &parent).unwrap();
        // The directory checked is the one removed, wherever it went; the copy is untouched.
        found.remove(".stack-checkout").unwrap();
        assert!(intact(&backup));
        assert!(!moved.join("data").exists() && !moved.join("data.stack-checkout").exists());
        assert_eq!(std::fs::read(moved.join("unsaved.txt")).unwrap(), b"keep me");

        // The data directory itself swapped for a copy after the check: its contents may go
        // with the checked directory's handle, but the copy's name is not removed for it.
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let found = held(hold(&entry));
        std::fs::rename(parent.join("data"), root.path().join("checked")).unwrap();
        copy(&root.path().join("checked"), &parent.join("data"));
        assert!(found.remove(".stack-checkout").unwrap_err().to_string().contains("replaced"));
        assert!(parent.join("data/rows").exists() && parent.join("data.stack-checkout").exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_checked_directory_is_moved_whole_and_a_replaced_one_is_not() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let to = root.path().join("elsewhere/data");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        held(hold(&entry)).move_to(&to, ".stack-checkout").unwrap();
        assert_eq!(std::fs::read(to.join("rows")).unwrap(), b"123");
        assert_eq!(locate(&to).1, entry.identity, "the same directory, not a copy");
        assert!(!parent.join("data").exists() && !parent.join("data.stack-checkout").exists());
        assert!(parent.join("unsaved.txt").exists());

        // Swapped for a copy after the check: the copy stays, and nothing moves.
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let found = held(hold(&entry));
        std::fs::rename(parent.join("data"), root.path().join("checked")).unwrap();
        copy(&root.path().join("checked"), &parent.join("data"));
        let to = root.path().join("target");
        assert!(found.move_to(&to, ".stack-checkout").unwrap_err().to_string().contains("replaced"));
        assert!(!to.exists() && parent.join("data/rows").exists() && parent.join("data.stack-checkout").exists());
    }

    #[cfg(unix)]
    #[test]
    fn size_and_removal_never_follow_links() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big"), vec![0u8; 1000]).unwrap();
        std::os::unix::fs::symlink(outside.path(), parent.join("data/link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("big"), parent.join("data/sub/file-link")).unwrap();
        let found = held(hold(&entry));
        assert_eq!(found.size(), 5);
        found.remove(".stack-checkout").unwrap();
        assert_eq!(std::fs::read(outside.path().join("big")).unwrap().len(), 1000);
    }

    /// Where each directory is mounted, as the kernel says, except the directories (by inode)
    /// a test puts on another mount: with its own device, or as a bind mount of the same
    /// filesystem (same device, another mount id), from the `after`-th question about it on.
    #[cfg(unix)]
    struct Mounted {
        at: Vec<(u64, bool, usize)>,
        asked: std::cell::RefCell<std::collections::HashMap<u64, usize>>,
    }

    #[cfg(unix)]
    impl Mounted {
        fn new(at: &[(&Path, bool, usize)]) -> Self {
            use std::os::unix::fs::MetadataExt;
            let at = at.iter().map(|(p, bind, after)| (std::fs::symlink_metadata(p).unwrap().ino(), *bind, *after)).collect();
            Self { at, asked: Default::default() }
        }
    }

    #[cfg(unix)]
    impl Mounted {
        fn answer(&self, real: imp::Mount, st: &libc::stat) -> imp::Mount {
            #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
            let ino = st.st_ino as u64;
            let mut asked = self.asked.borrow_mut();
            let n = asked.entry(ino).or_default();
            *n += 1;
            match self.at.iter().find(|(i, _, after)| *i == ino && *n > *after) {
                Some((_, true, _)) => imp::Mount { id: Some(real.id.unwrap_or(0) + 1000), ..real },
                Some((_, false, _)) => imp::Mount { dev: real.dev.wrapping_add(1), ..real },
                None => real,
            }
        }
    }

    #[cfg(unix)]
    impl imp::Boundary for Mounted {
        fn mount(&self, dir: &std::os::fd::OwnedFd, st: &libc::stat) -> std::io::Result<imp::Mount> {
            Ok(self.answer(imp::Kernel.mount(dir, st)?, st))
        }
        fn entry_mount(&self, dir: &std::os::fd::OwnedFd, name: &std::ffi::CStr, st: &libc::stat) -> std::io::Result<imp::Mount> {
            Ok(self.answer(imp::Kernel.entry_mount(dir, name, st)?, st))
        }
    }

    /// The kernel's answers, except that listing the directory with the inode of `at` fails
    /// (as `readdir` does on an I/O error) from the `after`-th time on.
    #[cfg(unix)]
    struct Unlistable {
        at: u64,
        after: usize,
        asked: std::cell::Cell<usize>,
    }

    #[cfg(unix)]
    impl imp::Boundary for Unlistable {
        fn mount(&self, dir: &std::os::fd::OwnedFd, st: &libc::stat) -> std::io::Result<imp::Mount> {
            imp::Kernel.mount(dir, st)
        }
        fn entry_mount(&self, dir: &std::os::fd::OwnedFd, name: &std::ffi::CStr, st: &libc::stat) -> std::io::Result<imp::Mount> {
            imp::Kernel.entry_mount(dir, name, st)
        }
        fn list(&self, dir: &std::os::fd::OwnedFd) -> std::io::Result<Vec<std::ffi::CString>> {
            use std::os::unix::fs::MetadataExt;
            let file = std::fs::File::from(dir.try_clone()?);
            if file.metadata()?.ino() == self.at {
                self.asked.set(self.asked.get() + 1);
                if self.asked.get() > self.after {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
            }
            imp::Kernel.list(dir)
        }
    }

    #[cfg(unix)]
    #[test]
    fn nothing_on_another_mount_inside_the_data_is_removed() {
        for bind in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (parent, entry) = recorded(root.path());
            let found = held(hold(&entry));
            // A mount at `data/sub`: refused before anything is removed.
            let mounted = Mounted::new(&[(&parent.join("data/sub"), bind, 0)]);
            assert_eq!(found.size_within(&mounted), 3, "what the mount holds is not counted");
            let e = found.remove_within(Some(".stack-checkout"), &mounted).unwrap_err().to_string();
            assert!(e.contains("sub inside it is a mount point") && e.contains("nothing was removed"), "{e}");
            assert!(e.contains(if bind { "of the same filesystem" } else { "device" }), "{e}");
            assert!(intact(&parent), "bind {bind}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_data_directory_that_is_itself_a_mount_point_is_not_emptied() {
        for bind in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (parent, entry) = recorded(root.path());
            let mounted = Mounted::new(&[(&parent.join("data"), bind, 0)]);
            let e = held(hold(&entry)).remove_within(Some(".stack-checkout"), &mounted).unwrap_err().to_string();
            assert!(e.contains("it is a mount point") && e.contains("nothing was removed"), "{e}");
            assert!(intact(&parent), "bind {bind}");
        }
    }

    /// A mount put at a path inside the data after the check that found none: the removal
    /// learns it from the directory it opened there, and stops before going into it.
    #[cfg(unix)]
    #[test]
    fn a_mount_put_inside_the_data_during_its_removal_stops_it_there() {
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        // `sub` is asked about once by the check, then again, now mounted, by the removal.
        let mounted = Mounted::new(&[(&parent.join("data/sub"), true, 1)]);
        let e = held(hold(&entry)).remove_within(Some(".stack-checkout"), &mounted).unwrap_err().to_string();
        assert!(e.contains("sub inside it is a mount point") && e.contains("removal stopped there"), "{e}");
        assert_eq!(std::fs::read(parent.join("data/sub/more")).unwrap(), b"45", "what the mount holds is untouched");
        assert!(parent.join("data.stack-checkout").exists(), "the marker stays with what is left");
    }

    /// Data removed by its path (snapshot restore's old data): the same refusal.
    #[cfg(unix)]
    #[test]
    fn a_tree_removed_by_its_path_is_never_removed_across_a_mount() {
        let root = tempfile::tempdir().unwrap();
        let (parent, _) = recorded(root.path());
        let mounted = Mounted::new(&[(&parent.join("data/sub"), true, 0)]);
        let data = parent.join("data").canonicalize().unwrap();
        let identity = locate(&data).1.unwrap();
        let e = imp::remove_tree_within(&data, identity, &mounted).unwrap_err().to_string();
        assert!(e.contains("mount point"), "{e}");
        assert!(intact(&parent));
        assert_eq!(remove_tree(&data, identity).unwrap(), 5);
        assert!(!parent.join("data").exists() && parent.join("data.stack-checkout").exists());
    }

    /// A single file mounted over inside the data (a bind mount of one file, or one from
    /// another filesystem): refused before anything is removed, or, mounted after the check,
    /// the removal stops at it. What a mount holds is not counted either.
    #[cfg(unix)]
    #[test]
    fn a_file_mounted_over_inside_the_data_is_never_removed() {
        for bind in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (parent, entry) = recorded(root.path());
            let mounted = Mounted::new(&[(&parent.join("data/sub/more"), bind, 0)]);
            let found = held(hold(&entry));
            assert_eq!(found.size_within(&mounted), 3, "what the mount holds is not counted");
            let e = found.remove_within(Some(".stack-checkout"), &mounted).unwrap_err().to_string();
            assert!(e.contains("more inside it is a mount point") && e.contains("nothing was removed"), "{e}");
            assert!(e.contains(if bind { "of the same filesystem" } else { "device" }), "{e}");
            assert!(intact(&parent), "bind {bind}");
        }
        // Mounted between the check and its removal: the check asked once, the removal again.
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let mounted = Mounted::new(&[(&parent.join("data/sub/more"), true, 1)]);
        let e = held(hold(&entry)).remove_within(Some(".stack-checkout"), &mounted).unwrap_err().to_string();
        assert!(e.contains("more inside it is a mount point") && e.contains("removal stopped there"), "{e}");
        assert_eq!(std::fs::read(parent.join("data/sub/more")).unwrap(), b"45", "the mounted file is untouched");
        assert!(parent.join("data.stack-checkout").exists());
    }

    /// A directory that cannot be listed whole (`readdir` failing), whether the data directory
    /// itself or one inside it: the check refuses before anything is removed, so nothing is
    /// removed on a check that did not finish. Failing only once the removal lists it again,
    /// the removal stops there.
    #[cfg(unix)]
    #[test]
    fn a_listing_that_fails_refuses_the_removal_before_anything_is_removed() {
        use std::os::unix::fs::MetadataExt;
        for at in ["data", "data/sub"] {
            let root = tempfile::tempdir().unwrap();
            let (parent, entry) = recorded(root.path());
            let failing = Unlistable { at: std::fs::metadata(parent.join(at)).unwrap().ino(), after: 0, asked: Default::default() };
            let e = held(hold(&entry)).remove_within(Some(".stack-checkout"), &failing).unwrap_err().to_string();
            assert!(e.contains("Input/output error") || e.contains("I/O error"), "{e}");
            assert!(intact(&parent), "{at}");
        }
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        let failing = Unlistable { at: std::fs::metadata(parent.join("data/sub")).unwrap().ino(), after: 1, asked: Default::default() };
        held(hold(&entry)).remove_within(Some(".stack-checkout"), &failing).unwrap_err();
        assert_eq!(std::fs::read(parent.join("data/sub/more")).unwrap(), b"45");
        assert!(parent.join("data.stack-checkout").exists(), "the marker stays with what is left");
    }

    /// A directory inside the data that cannot be opened (no permission): refused before
    /// anything is removed.
    #[cfg(unix)]
    #[test]
    fn a_directory_inside_that_cannot_be_opened_refuses_the_removal() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (parent, entry) = recorded(root.path());
        std::fs::set_permissions(parent.join("data/sub"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let removed = held(hold(&entry)).remove(".stack-checkout");
        std::fs::set_permissions(parent.join("data/sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(removed.is_err());
        assert!(intact(&parent));
    }

    /// A tree removed by its path (a snapshot restore's old data, or its copy) is reached with no
    /// link followed: a link anywhere above it refuses it, and a path that names another
    /// directory now than the one recorded (a stale path) is left alone.
    #[cfg(unix)]
    #[test]
    fn a_tree_removed_by_its_path_follows_no_link_and_is_only_the_recorded_directory() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let (parent, _) = recorded(&base);
        let data = parent.join("data");
        let identity = locate(&data).1.unwrap();
        std::os::unix::fs::symlink(&parent, base.join("link")).unwrap();
        let e = remove_tree(&base.join("link/data"), identity).unwrap_err().to_string();
        assert!(e.contains("is now a link"), "{e}");
        assert!(intact(&parent));
        let e = remove_tree(&parent.join("../parent/data"), identity).unwrap_err().to_string();
        assert!(e.contains("not a resolved path"), "{e}");
        // Another directory where the recorded one was.
        std::fs::rename(&data, base.join("moved")).unwrap();
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("new"), b"x").unwrap();
        let e = remove_tree(&data, identity).unwrap_err().to_string();
        assert!(e.contains("not the directory to remove"), "{e}");
        assert!(data.join("new").exists() && base.join("moved/rows").exists());
        assert_eq!(remove_tree(&base.join("nothing"), identity).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    /// The kernel's answer for directories on one filesystem: one mount, so the data is removed.
    #[cfg(unix)]
    #[test]
    fn directories_on_one_mount_are_on_the_same_mount() {
        use std::os::fd::{FromRawFd, OwnedFd};
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        let open = |p: &Path| {
            let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: open with a NUL-terminated path; the descriptor is owned here.
            unsafe { OwnedFd::from_raw_fd(libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)) }
        };
        let mount = |p: &Path| {
            let fd = open(p);
            // SAFETY: fstat with a valid descriptor and local storage.
            let st = unsafe {
                let mut st: libc::stat = std::mem::zeroed();
                assert_eq!(libc::fstat(std::os::fd::AsRawFd::as_raw_fd(&fd), &mut st), 0);
                st
            };
            imp::Boundary::mount(&imp::Kernel, &fd, &st).unwrap()
        };
        assert_eq!(mount(root.path()), mount(&root.path().join("a/b")));
        #[cfg(target_os = "linux")]
        assert!(mount(root.path()).id.is_some(), "Linux tells mounts apart");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(mount(root.path()).id, None);
    }

    #[cfg(unix)]
    #[test]
    fn bind_records_the_directory_once_it_exists() {
        let state = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("later/data");
        let (location, identity) = locate(&dir);
        assert_eq!((location.as_deref(), identity), (Some(root.path().canonicalize().unwrap().join("later/data").as_path()), None));
        record(state.path(), Entry { location, identity, ..entry("/a", "pg", dir.to_str().unwrap(), "t1") }).unwrap();
        bind(state.path(), &dir, "t1").unwrap();
        assert!(entries(state.path()).unwrap()[0].identity.is_none(), "nothing to bind yet");
        std::fs::create_dir_all(&dir).unwrap();
        bind(state.path(), &dir, "other-checkout").unwrap();
        assert!(entries(state.path()).unwrap()[0].identity.is_none(), "another checkout's");
        bind(state.path(), &dir, "t1").unwrap();
        assert_eq!(entries(state.path()).unwrap()[0].identity, locate(&dir).1);
        assert!(locate(&dir).1.is_some());
    }
}
