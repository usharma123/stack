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
}

/// A service's supervisor identity: the Pitchfork binary, its state directory and the daemon id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supervisor {
    pub pitchfork: PathBuf,
    pub state_dir: PathBuf,
    pub id: String,
}

fn paths(state: &Path) -> (PathBuf, PathBuf) {
    (state.join("data.json"), state.join("data.lock"))
}

/// Record (or replace) where `entry`'s service keeps its data.
pub fn record(state: &Path, entry: Entry) -> Result<()> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    if ledger.entries.contains(&entry) {
        return Ok(());
    }
    ledger.entries.retain(|e| !(e.project == entry.project && e.service == entry.service) && e.dir != entry.dir);
    ledger.entries.push(entry);
    write_json(&file, &ledger)
}

/// Every recorded entry.
pub fn entries(state: &Path) -> Result<Vec<Entry>> {
    Ok(read_json::<Ledger>(&paths(state).0)?.entries)
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
    let (location, identity) = locate(dir);
    if identity.is_none() {
        return Ok(());
    }
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let mut changed = false;
    for entry in ledger.entries.iter_mut().filter(|e| e.dir == dir && e.checkout == checkout) {
        if (entry.location.as_ref(), entry.identity) != (location.as_ref(), identity) {
            (entry.location, entry.identity) = (location.clone(), identity);
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
pub use imp::Held;

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

    pub fn hold(location: &Path, identity: [u64; 2]) -> Found {
        let (Some(parent_path), Some(name)) = (location.parent(), location.file_name()) else {
            return Found::Other(format!("{} is not a data directory", location.display()));
        };
        let root = match open_at(None, c"/", libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(fd) => fd,
            Err(e) => return Found::Other(format!("cannot open / ({e})")),
        };
        let mut parent = root;
        let mut walked = std::path::PathBuf::from("/");
        for component in parent_path.components() {
            let part = match component {
                Component::RootDir => continue,
                Component::Normal(part) => part,
                _ => return Found::Other(format!("{} is not a resolved path", location.display())),
            };
            walked.push(part);
            let Ok(part) = cstring(part) else { return Found::Other(format!("{} cannot be named", walked.display())) };
            parent = match open_dir(&parent, &part, &walked) {
                Ok(Some(fd)) => fd,
                Ok(None) => return Found::Gone,
                Err(why) => return Found::Other(why),
            };
        }
        let Ok(name) = cstring(name) else { return Found::Other(format!("{} cannot be named", location.display())) };
        let dir = match open_dir(&parent, &name, location) {
            Ok(Some(fd)) => fd,
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

        /// Bytes of regular files in the directory, never following links.
        pub fn size(&self) -> u64 {
            walk(&self.dir, false).unwrap_or(0)
        }

        /// Remove everything in the held directory, then the directory and the `suffix` file
        /// beside it from the held parent, provided the name there still is this directory.
        /// Returns the bytes of regular files removed.
        pub fn remove(self, suffix: &str) -> io::Result<u64> {
            let bytes = walk(&self.dir, true)?;
            let now = stat_at(&self.parent, &self.name)?;
            if id(&now) != self.identity {
                return Err(io::Error::other("the directory was replaced while it was being removed; what replaced it is left alone"));
            }
            // SAFETY: unlinkat with the held parent and a NUL-terminated name.
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                return Err(io::Error::last_os_error());
            }
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
                let entry = libc::readdir(stream);
                if entry.is_null() {
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

    /// Bytes of regular files under `dir`, removing everything under it when `remove`. Links
    /// are never followed: a link is removed (or counted as nothing) itself.
    fn walk(dir: &OwnedFd, remove: bool) -> io::Result<u64> {
        let mut bytes = 0;
        for name in names(dir)? {
            let st = match stat_at(dir, &name) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                other => other?,
            };
            let directory = is(&st, libc::S_IFDIR);
            if directory {
                let child = open_at(Some(dir), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
                bytes += walk(&child, remove)?;
            } else if is(&st, libc::S_IFREG) {
                bytes += u64::try_from(st.st_size).unwrap_or(0);
            }
            if remove {
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
impl Held {
    pub fn beside(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
    pub fn size(&self) -> u64 {
        0
    }
    pub fn remove(self, _: &str) -> std::io::Result<u64> {
        Err(std::io::Error::other("not supported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(project: &str, service: &str, dir: &str, checkout: &str) -> Entry {
        Entry { project: project.into(), service: service.into(), dir: dir.into(), checkout: checkout.into(), supervisor: None, location: None, identity: None }
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
