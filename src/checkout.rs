//! Which directory a checkout is, beyond its path.
//!
//! Agents delete worktrees and create new ones at the same path all the time, and everything
//! keyed by path alone (mise's daemon state, so each preset's data) would pass from one to the
//! next. A checkout's token is derived from its path together with the directory's device,
//! inode and, where the filesystem records one, creation time: the same directory always has
//! the same token, across restarts and without any state of its own, and a directory recreated
//! at that path has another. Filesystems without creation times can reuse an inode, so there a
//! recreated directory may still match; nothing better can be known about it.
//!
//! The token is given to every service in its own environment (never to `exec` or `run`
//! commands), so a process carrying it was launched by the supervisor for this checkout, and it
//! marks the data directories this checkout's services use. Commands get a different value
//! derived from it ([`command_mark`]). It tells checkouts apart; it is not a credential.

use crate::hash::sha256_hex;
use std::path::Path;

/// The variable through which each service receives its checkout's token.
pub const TOKEN_VAR: &str = "STACK_CHECKOUT_TOKEN";

/// The variable through which every command `exec`, `run` and the MCP server start for a
/// checkout receives its [`command_mark`], so `down` can find what such a command left running
/// after it ended (see `crate::commands`). Services never get it (their environment drops every
/// `STACK_*` variable), so a process carrying it is a command, never a service, and is never
/// stopped as one.
pub const COMMAND_VAR: &str = "STACK_COMMAND_MARK";

/// What identifies the directory at `root` beyond its path: its device, inode and, where the
/// filesystem records one, creation time. A move keeps it (`stack mv`), and a directory
/// recreated at the same path gets another (except on a filesystem without creation times that
/// reuses the inode). Commands are registered under it (see `crate::commands`).
pub fn identity(root: &Path) -> Option<String> {
    let root = &root.canonicalize().ok()?;
    let (dev, ino) = crate::session::dir_id(root)?;
    Some(sha256_hex(format!("checkout\0{dev}\0{ino}\0{}", created(root)).as_bytes())[..32].to_string())
}

/// The mark of the commands Stack starts for the checkout at `root`, derived from its
/// [`identity`]: one per checkout, kept by a move, and never equal to its token.
pub fn command_mark(root: &Path) -> Option<String> {
    identity(root).map(|id| command_mark_of(&id))
}

/// [`command_mark`] for a checkout's [`identity`].
pub fn command_mark_of(identity: &str) -> String {
    sha256_hex(format!("stack-command\0{identity}").as_bytes())[..32].to_string()
}

fn created(dir: &Path) -> String {
    std::fs::metadata(dir)
        .ok()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(String::new, |d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
}

/// The token of the directory at `root` now, if it exists, whichever way its path is spelled.
pub fn token(root: &Path) -> Option<String> {
    let root = &root.canonicalize().ok()?;
    token_as(root, root)
}

/// The token the directory now at `dir` had at `path`, the resolved path it was moved from:
/// a move keeps a directory's device, inode and creation time, and only its path changes.
pub fn token_as(path: &Path, dir: &Path) -> Option<String> {
    let (dev, ino) = crate::session::dir_id(dir)?;
    let created = created(dir);
    Some(sha256_hex(format!("{}\0{dev}\0{ino}\0{created}", path.display()).as_bytes())[..32].to_string())
}

/// Whether a checkout recorded at a path is still there, as far as can be shown.
///
/// A path that cannot be found is not proof the checkout was deleted: a lookup refused for
/// permission or failing for I/O says nothing, and neither does a missing path on a volume that
/// is not mounted right now (a disk unplugged, a network share down, a mount point left empty).
/// Only [`Presence::Absent`] lets Stack act on a checkout being gone (reclaim its session, data,
/// ports and identities); [`Presence::Unknown`] leaves everything as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Something is at the path.
    Present,
    /// Nothing is at the path, and the filesystem the checkout was on is there to say so.
    Absent,
    /// Whether the checkout is gone cannot be shown: why, said for a person.
    Unknown(String),
}

/// Whether [`presence`] shows the checkout recorded at `path` on `device` is gone: the only
/// answer on which Stack drops what it keeps for a checkout.
pub fn gone(path: &Path, device: Option<u64>) -> bool {
    presence(path, device) == Presence::Absent
}

/// The device of the filesystem the directory at `root` is on now, which Stack records with
/// whatever it keeps for a checkout, so that once the path is gone a deletion can be told from
/// that filesystem not being mounted (see [`presence`]).
pub fn device(root: &Path) -> Option<u64> {
    crate::session::dir_id(root).map(|(dev, _)| dev)
}

/// Whether the checkout recorded at `path`, on the filesystem with `device` (see [`device`];
/// `None` for records an earlier Stack kept), is still there.
///
/// It is [`Presence::Absent`] only when the path is not found and the deepest directory above it
/// that is there is on the checkout's own filesystem: that filesystem is mounted and has nothing
/// at the path. With no device recorded, only when the path's own parent is there, can be listed
/// and holds something (a mount point whose volume is not mounted is left empty). Anything else
/// is [`Presence::Unknown`].
pub fn presence(path: &Path, device: Option<u64>) -> Presence {
    presence_with(&Disk, path, device)
}

/// What [`presence`] looks at, so its decisions can be shown without unmounting anything.
pub(crate) trait Lookup {
    /// The device of what `path` resolves to, and whether it is a directory.
    fn stat(&self, path: &Path) -> std::io::Result<(u64, bool)>;
    /// Whether the directory at `path` holds any entry.
    fn has_entries(&self, path: &Path) -> std::io::Result<bool>;
}

struct Disk;

impl Lookup for Disk {
    fn stat(&self, path: &Path) -> std::io::Result<(u64, bool)> {
        let meta = std::fs::metadata(path)?;
        #[cfg(unix)]
        let dev = std::os::unix::fs::MetadataExt::dev(&meta);
        #[cfg(not(unix))]
        let dev = 0;
        Ok((dev, meta.is_dir()))
    }

    fn has_entries(&self, path: &Path) -> std::io::Result<bool> {
        Ok(std::fs::read_dir(path)?.next().transpose()?.is_some())
    }
}

/// A lookup error that says nothing is there: not found, or a component that is not a directory.
fn not_there(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR)
}

pub(crate) fn presence_with(fs: &dyn Lookup, path: &Path, device: Option<u64>) -> Presence {
    let shown = path.display();
    match fs.stat(path) {
        Ok(_) => return Presence::Present,
        Err(e) if not_there(&e) => {}
        Err(e) => return Presence::Unknown(format!("{shown} cannot be looked up ({e})")),
    }
    // The deepest directory above the path that is there.
    let mut above = path.parent();
    let (found, dev) = loop {
        let Some(dir) = above else {
            return Presence::Unknown(format!("{shown} is not found, and no directory above it is either"));
        };
        match fs.stat(dir) {
            Ok((dev, true)) => break (dir, dev),
            Ok((_, false)) => above = dir.parent(),
            Err(e) if not_there(&e) => above = dir.parent(),
            Err(e) => return Presence::Unknown(format!("{shown} is not found, but {} cannot be looked up ({e})", dir.display())),
        }
    };
    match device {
        Some(recorded) if dev == recorded => Presence::Absent,
        Some(recorded) => Presence::Unknown(format!(
            "{shown} is not found, but {} is on another filesystem (device {dev}) than the checkout was (device {recorded}): \
             its volume may not be mounted right now",
            found.display()
        )),
        None if Some(found) != path.parent() => Presence::Unknown(format!(
            "{shown} is not found and neither is the directory holding it, and Stack recorded no filesystem for it to tell \
             a deletion from a volume that is not mounted"
        )),
        None => match fs.has_entries(found) {
            Ok(true) => Presence::Absent,
            Ok(false) => Presence::Unknown(format!(
                "{shown} is not found, and {} is empty, as a mount point whose volume is not mounted is; Stack recorded no \
                 filesystem for it to tell the two apart",
                found.display()
            )),
            Err(e) => Presence::Unknown(format!("{shown} is not found, but {} cannot be listed ({e})", found.display())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io;
    use std::path::PathBuf;

    /// A filesystem as [`presence`] would find it, without mounting or unmounting anything: each
    /// path's lookup answer (device and whether a directory, or an OS error), and which
    /// directories hold something. Paths not listed are not found.
    #[derive(Default)]
    struct Seen {
        stat: HashMap<PathBuf, Result<(u64, bool), i32>>,
        entries: HashMap<PathBuf, Result<bool, i32>>,
    }

    impl Seen {
        fn dir(mut self, path: &str, dev: u64, holds_something: bool) -> Self {
            self.stat.insert(path.into(), Ok((dev, true)));
            self.entries.insert(path.into(), Ok(holds_something));
            self
        }
        fn file(mut self, path: &str, dev: u64) -> Self {
            self.stat.insert(path.into(), Ok((dev, false)));
            self
        }
        fn failing(mut self, path: &str, errno: i32) -> Self {
            self.stat.insert(path.into(), Err(errno));
            self
        }
        fn unlistable(mut self, path: &str, errno: i32) -> Self {
            self.entries.insert(path.into(), Err(errno));
            self
        }
    }

    impl Lookup for Seen {
        fn stat(&self, path: &Path) -> io::Result<(u64, bool)> {
            match self.stat.get(path) {
                Some(Ok(found)) => Ok(*found),
                Some(Err(errno)) => Err(io::Error::from_raw_os_error(*errno)),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
        fn has_entries(&self, path: &Path) -> io::Result<bool> {
            match self.entries.get(path) {
                Some(Ok(found)) => Ok(*found),
                Some(Err(errno)) => Err(io::Error::from_raw_os_error(*errno)),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
    }

    fn unknown(found: Presence) -> String {
        match found {
            Presence::Unknown(why) => why,
            other => panic!("{other:?}"),
        }
    }

    const ROOT_FS: u64 = 1;
    const VOLUME: u64 = 7;

    #[test]
    fn a_checkout_is_gone_only_when_its_own_filesystem_is_there_to_say_so() {
        let fs = || Seen::default().dir("/", ROOT_FS, true).dir("/home", ROOT_FS, true).dir("/home/me", ROOT_FS, true);
        let wt = Path::new("/home/me/wt");
        // There, whatever is there.
        assert_eq!(presence_with(&fs().dir("/home/me/wt", ROOT_FS, true), wt, Some(ROOT_FS)), Presence::Present);
        // Deleted, its parent there on its filesystem; or the parent deleted with it.
        assert_eq!(presence_with(&fs(), wt, Some(ROOT_FS)), Presence::Absent);
        assert_eq!(presence_with(&fs(), Path::new("/home/me/gone/wt"), Some(ROOT_FS)), Presence::Absent);
        // A file where a directory above it was: nothing can be at the path.
        assert_eq!(presence_with(&fs().file("/home/me/gone", ROOT_FS).failing("/home/me/gone/wt", libc::ENOTDIR), Path::new("/home/me/gone/wt"), Some(ROOT_FS)), Presence::Absent);
        // A lookup refused or failing says nothing, whether of the path or of a directory above.
        assert!(unknown(presence_with(&fs().failing("/home/me/wt", libc::EACCES), wt, Some(ROOT_FS))).contains("cannot be looked up"));
        assert!(unknown(presence_with(&fs().failing("/home/me/wt", libc::EIO), wt, Some(ROOT_FS))).contains("cannot be looked up"));
        assert!(unknown(presence_with(&fs().failing("/home/me", libc::EACCES), wt, Some(ROOT_FS))).contains("/home/me cannot be looked up"));
        assert!(unknown(presence_with(&Seen::default(), wt, Some(ROOT_FS))).contains("no directory above it"));
    }

    #[test]
    fn a_checkout_on_a_volume_that_is_not_mounted_is_not_taken_for_deleted() {
        // macOS: the volume's directory under /Volumes goes with it.
        let mac = Seen::default().dir("/", ROOT_FS, true).dir("/Volumes", ROOT_FS, true);
        let why = unknown(presence_with(&mac, Path::new("/Volumes/ext/repos/wt"), Some(VOLUME)));
        assert!(why.contains("/Volumes is on another filesystem") && why.contains("may not be mounted"), "{why}");
        // Linux: the mount point stays, empty, on the filesystem above it.
        let linux = Seen::default().dir("/", ROOT_FS, true).dir("/mnt", ROOT_FS, true).dir("/mnt/ext", ROOT_FS, false);
        assert!(unknown(presence_with(&linux, Path::new("/mnt/ext/wt"), Some(VOLUME))).contains("/mnt/ext is on another filesystem"));
        assert!(unknown(presence_with(&linux, Path::new("/mnt/ext/repos/wt"), Some(VOLUME))).contains("/mnt/ext is on another filesystem"));
        // Mounted again, the checkout deleted from it: gone.
        let mounted = Seen::default().dir("/", ROOT_FS, true).dir("/mnt", ROOT_FS, true).dir("/mnt/ext", VOLUME, true);
        assert_eq!(presence_with(&mounted, Path::new("/mnt/ext/wt"), Some(VOLUME)), Presence::Absent);
        assert_eq!(presence_with(&mounted, Path::new("/mnt/ext/repos/wt"), Some(VOLUME)), Presence::Absent);
    }

    #[test]
    fn a_record_without_a_device_is_gone_only_beside_something_in_its_own_parent() {
        let fs = Seen::default().dir("/", ROOT_FS, true).dir("/mnt", ROOT_FS, true).dir("/mnt/ext", ROOT_FS, false).dir("/home", ROOT_FS, true).dir("/home/me", ROOT_FS, true);
        assert_eq!(presence_with(&fs, Path::new("/home/me/wt"), None), Presence::Absent);
        // An empty parent is what a mount point left by its volume looks like.
        assert!(unknown(presence_with(&fs, Path::new("/mnt/ext/wt"), None)).contains("is empty"));
        // Its parent gone too: a volume's directory, or a deleted tree, cannot be told apart.
        assert!(unknown(presence_with(&fs, Path::new("/home/me/gone/wt"), None)).contains("neither is the directory holding it"));
        assert!(unknown(presence_with(&fs.unlistable("/home/me", libc::EACCES), Path::new("/home/me/wt"), None)).contains("cannot be listed"));
    }

    /// The same decisions on the disk: a refused lookup, a recorded device that is not the one
    /// above the path (as for an unmounted volume), and a plain deletion.
    #[cfg(unix)]
    #[test]
    fn on_disk_a_refused_lookup_is_unknown_and_a_deletion_is_gone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let wt = parent.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(parent.join("sibling"), b"").unwrap();
        let dev = device(&wt).unwrap();
        assert_eq!(presence(&wt, Some(dev)), Presence::Present);
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
            let refused = presence(&wt, Some(dev));
            let refused_legacy = presence(&wt, None);
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(unknown(refused).contains("cannot be looked up"));
            assert!(unknown(refused_legacy).contains("cannot be looked up"));
            assert!(!gone(&wt, Some(dev)));
        }
        std::fs::remove_dir(&wt).unwrap();
        assert_eq!(presence(&wt, Some(dev)), Presence::Absent);
        assert!(gone(&wt, Some(dev)) && gone(&wt, None));
        assert!(unknown(presence(&wt, Some(dev.wrapping_add(1)))).contains("on another filesystem"));
    }

    #[test]
    fn a_directory_keeps_its_token_and_one_recreated_at_its_path_gets_another() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let first = token(&root).unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(token(&root).unwrap(), first, "stable for the same directory");
        // Hold the old inode so the filesystem cannot hand it to the new directory.
        let held = parent.path().join("held");
        std::fs::rename(&root, &held).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert_ne!(token(&root).unwrap(), first, "a new directory at the same path");
        assert!(token(&parent.path().join("missing")).is_none());
    }

    #[test]
    fn a_moved_directory_had_the_token_its_old_path_gives_it() {
        let parent = tempfile::tempdir().unwrap();
        let (old, new) = (parent.path().canonicalize().unwrap().join("old"), parent.path().canonicalize().unwrap().join("new"));
        std::fs::create_dir(&old).unwrap();
        let before = token(&old).unwrap();
        std::fs::rename(&old, &new).unwrap();
        assert_eq!(token_as(&old, &new).as_deref(), Some(before.as_str()));
        assert_ne!(token(&new).unwrap(), before, "the path is part of it");
    }
}
