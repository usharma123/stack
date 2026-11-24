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
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
/// is not mounted right now (a disk unplugged, a network share down, a mount point left empty,
/// a bind mount taken down). Only [`Presence::Absent`] lets Stack act on a checkout being gone
/// (reclaim its session, data, ports and identities); [`Presence::Unknown`] leaves everything as
/// it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Something is at the path.
    Present,
    /// Nothing is at the path, and the mount the checkout was on is there to say so.
    Absent,
    /// Whether the checkout is gone cannot be shown: why, said for a person.
    Unknown(String),
}

/// Where a checkout was when Stack recorded something for it, kept with that record (its
/// session, port reservations, identity tokens, data records and supervisor) so that once its
/// path is gone a deletion can be told from the mount it was on not being there (see
/// [`presence`]). Both fields are absent in records an earlier Stack kept; `location` is absent
/// in those Stack 0.1.24 or earlier kept, and wherever it could not be learned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    /// The device of the filesystem the checkout was on.
    #[serde(rename = "project_device", default, skip_serializing_if = "Option::is_none")]
    pub device: Option<u64>,
    /// The mount it was on (see [`Location`]).
    #[serde(rename = "project_location", default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

impl Recorded {
    /// Where the checkout at `root` is now, as far as can be learned.
    pub fn of(root: &Path) -> Self {
        Self { device: device(root), location: location(root) }
    }

    /// `self`, learned now, keeping the location `previous` recorded when none could be learned
    /// now: evidence is never dropped for want of it (a mismatch keeps the checkout unknown).
    pub fn or_kept(self, previous: &Recorded) -> Self {
        match self.location {
            Some(_) => self,
            None => Self { location: previous.location.clone(), ..self },
        }
    }
}

/// The mount a checkout was on, which a missing path is judged by: the root of that mount (the
/// highest directory at or above the checkout on the same mount, as a resolved path), which
/// directory that root is (device, inode and, where the filesystem records one, birth time),
/// and, where mounts can share a device (Linux bind mounts, see [`crate::mounts::BINDS`]), the
/// kernel's id of the mount together with the boot and mount namespace in which alone that id
/// means it (ids are reused once a mount is gone, and differ after a restart).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub mount_root: PathBuf,
    /// The mount root's device and inode.
    pub root: [u64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_born: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<MountInstance>,
}

/// One mount as the kernel named it: its id (reused once it is gone), its unique id where the
/// kernel has one (Linux 6.8: never reused within a boot), and the boot and mount namespace it
/// was read in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountInstance {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<u64>,
    pub boot: String,
    pub namespace: u64,
}

/// Whether [`presence`] shows the checkout recorded at `path` is gone: the only answer on which
/// Stack drops what it keeps for a checkout.
pub fn gone(path: &Path, recorded: &Recorded) -> bool {
    presence(path, recorded) == Presence::Absent
}

/// The device of the filesystem the directory at `root` is on now (see [`Recorded`]).
pub fn device(root: &Path) -> Option<u64> {
    crate::session::dir_id(root).map(|(dev, _)| dev)
}

/// The mount the directory at `root` is on now (see [`Location`]); `None` when it is not there
/// or, where mounts can share a device, the kernel will not say which mount it is on.
pub fn location(root: &Path) -> Option<Location> {
    location_with(&Disk, root)
}

/// Whether the checkout recorded at `path`, as `recorded` (see [`Recorded`]), is still there.
///
/// It is [`Presence::Absent`] only when the path is not found and the deepest directory above it
/// that is there is on the very mount the checkout was on:
/// - its mount's root is the recorded one, at the same path, the same directory (device, inode,
///   and birth time where both are known), so neither another filesystem nor the directory a
///   mount was on (an unmounted volume's empty mount point, the directory under a bind mount
///   taken down, even of the same filesystem) is taken for it;
/// - where mounts can share a device (Linux), it is also the same mount: in the boot and mount
///   namespace the record was made in, by the kernel's mount id (the unique id where both sides
///   have it). Recorded in another boot or namespace, or one that cannot be read, the mount's root
///   must have a recorded birth time that matches, the only thing that recognises it across a
///   restart; otherwise it is unknown.
///
/// A record without a location (an earlier Stack's, or where none could be learned) is judged
/// by its device alone, and only where every mount has a device of its own (not Linux): there a
/// bind mount of the same filesystem that is not mounted right now leaves the same thing behind
/// as a deletion, so it is unknown. A record with neither is unknown everywhere: an empty or a
/// full parent directory says nothing about a mount that is not there. Such a record is
/// completed (see [`Refresh`]) while its checkout is there.
pub fn presence(path: &Path, recorded: &Recorded) -> Presence {
    presence_with(&Disk, path, recorded)
}

/// What [`presence`] and [`location`] look at, so their decisions can be shown without mounting
/// anything.
pub(crate) trait Lookup {
    /// What `path` resolves to.
    fn look(&self, path: &Path) -> std::io::Result<Node>;
    /// `path` (which is there) with every link resolved.
    fn resolve(&self, path: &Path) -> std::io::Result<PathBuf>;
    /// This boot and this process's mount namespace, as far as they can be read.
    fn instance(&self) -> (Option<String>, Option<u64>);
    /// Whether mounts can share a device here (see [`crate::mounts::BINDS`]).
    fn binds(&self) -> bool;
}

/// A directory (or anything else) as [`Lookup`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub dev: u64,
    pub ino: u64,
    pub dir: bool,
    pub born: Option<String>,
    /// The mount it is on, where mounts have ids.
    pub mount: Option<crate::mounts::Ids>,
}

struct Disk;

impl Lookup for Disk {
    fn look(&self, path: &Path) -> std::io::Result<Node> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path)?;
        let born = meta.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()));
        let mount = if crate::mounts::BINDS { crate::mounts::of_path(path)? } else { None };
        Ok(Node { dev: meta.dev(), ino: meta.ino(), dir: meta.is_dir(), born, mount })
    }

    fn resolve(&self, path: &Path) -> std::io::Result<PathBuf> {
        path.canonicalize()
    }

    fn instance(&self) -> (Option<String>, Option<u64>) {
        (crate::procinfo::boot_id(), crate::mounts::namespace())
    }

    fn binds(&self) -> bool {
        crate::mounts::BINDS
    }
}

/// A lookup error that says nothing is there: not found, or a component that is not a directory.
fn not_there(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR)
}

/// Whether `a` and `b` are on one mount: one device and, where mounts can share one, one mount
/// id (the unique one where both have it). Without ids where they are needed, never.
fn same_mount(a: &Node, b: &Node, binds: bool) -> bool {
    a.dev == b.dev
        && (!binds
            || match (a.mount, b.mount) {
                (Some(x), Some(y)) => match (x.unique, y.unique) {
                    (Some(p), Some(q)) => p == q,
                    _ => x.id == y.id,
                },
                _ => false,
            })
}

/// The root of the mount the directory `dir` (resolved, found as `node`) is on: the highest
/// directory at or above it on the same mount, and what it is.
fn mount_root(fs: &dyn Lookup, dir: &Path, node: &Node) -> Result<(PathBuf, Node), String> {
    if fs.binds() && node.mount.is_none() {
        return Err(format!("which mount {} is on cannot be told", dir.display()));
    }
    let mut root = (dir.to_path_buf(), node.clone());
    let mut above = dir.parent();
    while let Some(parent) = above {
        let found = fs.look(parent).map_err(|e| format!("{} cannot be looked up ({e})", parent.display()))?;
        if !same_mount(node, &found, fs.binds()) {
            break;
        }
        root = (parent.to_path_buf(), found);
        above = parent.parent();
    }
    Ok(root)
}

pub(crate) fn location_with(fs: &dyn Lookup, root: &Path) -> Option<Location> {
    let root = fs.resolve(root).ok()?;
    let node = fs.look(&root).ok().filter(|n| n.dir)?;
    let (mount_root, top) = mount_root(fs, &root, &node).ok()?;
    let mount = match (fs.binds(), node.mount, fs.instance()) {
        (true, Some(ids), (Some(boot), Some(namespace))) => Some(MountInstance { id: ids.id, unique: ids.unique, boot, namespace }),
        _ => None,
    };
    Some(Location { mount_root, root: [top.dev, top.ino], root_born: top.born, mount })
}

pub(crate) fn presence_with(fs: &dyn Lookup, path: &Path, recorded: &Recorded) -> Presence {
    let shown = path.display();
    match fs.look(path) {
        Ok(_) => return Presence::Present,
        Err(e) if not_there(&e) => {}
        Err(e) => return Presence::Unknown(format!("{shown} cannot be looked up ({e})")),
    }
    // The deepest directory above the path that is there.
    let mut above = path.parent();
    let (found, node) = loop {
        let Some(dir) = above else {
            return Presence::Unknown(format!("{shown} is not found, and no directory above it is either"));
        };
        match fs.look(dir) {
            Ok(node) if node.dir => break (dir, node),
            Ok(_) => above = dir.parent(),
            Err(e) if not_there(&e) => above = dir.parent(),
            Err(e) => return Presence::Unknown(format!("{shown} is not found, but {} cannot be looked up ({e})", dir.display())),
        }
    };
    if let Some(location) = &recorded.location {
        return on_recorded_mount(fs, path, found, &node, location);
    }
    match recorded.device {
        Some(device) if fs.binds() => Presence::Unknown(format!(
            "{shown} is not found, and Stack recorded only the device of the filesystem it was on (device {device}), not \
             the mount: a bind mount of that filesystem that is not mounted right now leaves the same behind as a deletion. \
             If it is mounted again, any Stack command records its mount; if it was deleted, recreate the directory, run \
             `stack gc`, and delete it again"
        )),
        Some(device) if node.dev == device => Presence::Absent,
        Some(device) => Presence::Unknown(format!(
            "{shown} is not found, but {} is on another filesystem (device {}) than the checkout was (device {device}): \
             its volume may not be mounted right now",
            found.display(),
            node.dev
        )),
        None => Presence::Unknown(format!(
            "{shown} is not found, and Stack recorded no filesystem for it (a record an earlier Stack kept), so a deletion \
             cannot be told from a volume or mount that is not there right now. If it was deleted, recreate the directory, \
             run `stack gc`, and delete it again"
        )),
    }
}

/// [`presence_with`] for a missing `path` whose deepest directory there is `found` (`node`),
/// judged by the mount the checkout was on.
fn on_recorded_mount(fs: &dyn Lookup, path: &Path, found: &Path, node: &Node, location: &Location) -> Presence {
    let shown = path.display();
    let elsewhere = |what: String| {
        Presence::Unknown(format!(
            "{shown} is not found, but {} is {what} than the checkout was on ({}): the mount it was on may not be there \
             right now",
            found.display(),
            location.mount_root.display()
        ))
    };
    if node.dev != location.root[0] {
        return elsewhere(format!("on another filesystem (device {}, not {})", node.dev, location.root[0]));
    }
    let resolved = match fs.resolve(found) {
        Ok(resolved) => resolved,
        Err(e) => return Presence::Unknown(format!("{shown} is not found, but {} cannot be resolved ({e})", found.display())),
    };
    let (root, top) = match mount_root(fs, &resolved, node) {
        Ok(root) => root,
        Err(why) => return Presence::Unknown(format!("{shown} is not found, but {why}")),
    };
    if root != location.mount_root || [top.dev, top.ino] != location.root {
        return elsewhere(format!("on another mount (rooted at {})", root.display()));
    }
    if let (Some(recorded), Some(now)) = (&location.root_born, &top.born) {
        if recorded != now {
            return elsewhere(format!("on another mount (its root {} was created at another time)", root.display()));
        }
    }
    if !fs.binds() {
        return Presence::Absent;
    }
    let (boot, namespace) = fs.instance();
    match &location.mount {
        Some(m) if boot.as_deref() == Some(m.boot.as_str()) && namespace == Some(m.namespace) => {
            let Some(ids) = node.mount else {
                return Presence::Unknown(format!("{shown} is not found, but which mount {} is on cannot be told", found.display()));
            };
            let same = match (m.unique, ids.unique) {
                (Some(recorded), Some(now)) => recorded == now,
                _ => m.id == ids.id,
            };
            if same {
                Presence::Absent
            } else {
                elsewhere("on another mount of the same directory (another mount id)".into())
            }
        }
        _ if location.root_born.is_some() && top.born.is_some() => Presence::Absent,
        _ => Presence::Unknown(format!(
            "{shown} is not found, and the mount it was on was recorded in another boot or mount namespace (or one that \
             cannot be read), where its id means nothing here, on a filesystem that records no birth time to recognise its \
             root by; a bind mount of the same directory that is not mounted right now cannot be ruled out"
        )),
    }
}

/// Completes and refreshes [`Recorded`] values of checkouts that are there: a record an earlier
/// Stack kept, or one from another boot or mount namespace, gets where the checkout is now, so
/// that once it is deleted that can be shown. Learned once per path.
#[derive(Default)]
pub struct Refresh(std::collections::HashMap<PathBuf, Option<Recorded>>);

impl Refresh {
    /// Bring `recorded`, kept for the checkout at `path`, up to where it is now, when something
    /// is at the path and its mount can be learned. Whether it changed.
    pub fn apply(&mut self, path: &Path, recorded: &mut Recorded) -> bool {
        let now = self.0.entry(path.to_path_buf()).or_insert_with(|| {
            let now = Recorded::of(path);
            now.location.is_some().then_some(now)
        });
        match now {
            Some(now) if now != recorded => {
                *recorded = now.clone();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mounts::Ids;
    use std::collections::HashMap;
    use std::io;

    /// A machine as [`presence`] and [`location`] would find it, without mounting or unmounting
    /// anything: each path's lookup answer (a node, or an OS error), this boot and mount
    /// namespace, and whether mounts can share a device. Paths not listed are not found; every
    /// path given is resolved already.
    struct Seen {
        nodes: HashMap<PathBuf, Result<Node, i32>>,
        boot: Option<&'static str>,
        namespace: Option<u64>,
        binds: bool,
    }

    const ROOT_FS: u64 = 1;
    const VOLUME: u64 = 7;
    const NS: u64 = 4026531840;

    impl Seen {
        /// Linux, in boot `a` and namespace [`NS`].
        fn linux() -> Self {
            Self { nodes: HashMap::new(), boot: Some("a"), namespace: Some(NS), binds: true }
        }
        /// macOS: every mount a device of its own.
        fn mac() -> Self {
            Self { nodes: HashMap::new(), boot: Some("a"), namespace: None, binds: false }
        }
        /// A directory: its device, inode, birth time, and mount id (unique id beside it).
        fn dir(mut self, path: &str, dev: u64, ino: u64, born: Option<&str>, mount: Option<(u64, Option<u64>)>) -> Self {
            let mount = mount.map(|(id, unique)| Ids { id, unique });
            self.nodes.insert(path.into(), Ok(Node { dev, ino, dir: true, born: born.map(String::from), mount }));
            self
        }
        fn file(mut self, path: &str, dev: u64) -> Self {
            self.nodes.insert(path.into(), Ok(Node { dev, ino: 99, dir: false, born: None, mount: None }));
            self
        }
        fn failing(mut self, path: &str, errno: i32) -> Self {
            self.nodes.insert(path.into(), Err(errno));
            self
        }
        fn gone(mut self, path: &str) -> Self {
            self.nodes.remove(Path::new(path));
            self
        }
    }

    impl Lookup for Seen {
        fn look(&self, path: &Path) -> io::Result<Node> {
            match self.nodes.get(path) {
                Some(Ok(node)) => Ok(node.clone()),
                Some(Err(errno)) => Err(io::Error::from_raw_os_error(*errno)),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
        fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
            self.look(path).map(|_| path.to_path_buf())
        }
        fn instance(&self) -> (Option<String>, Option<u64>) {
            (self.boot.map(String::from), self.namespace)
        }
        fn binds(&self) -> bool {
            self.binds
        }
    }

    fn unknown(found: Presence) -> String {
        match found {
            Presence::Unknown(why) => why,
            other => panic!("{other:?}"),
        }
    }

    /// What `fs` would have Stack record for `path`.
    fn recorded(fs: &Seen, path: &str) -> Recorded {
        let location = location_with(fs, Path::new(path));
        let device = fs.look(Path::new(path)).ok().map(|n| n.dev);
        Recorded { device, location }
    }

    /// Linux: `/` (mount 20) holding `/home/me`, and `/srv/repos`, a directory of the same
    /// filesystem bind-mounted at `/home/me/repos` (mount 50) with the checkout `wt` in it.
    fn bound() -> Seen {
        Seen::linux()
            .dir("/", ROOT_FS, 2, Some("mkfs"), Some((20, Some(1020))))
            .dir("/home", ROOT_FS, 3, None, Some((20, Some(1020))))
            .dir("/home/me", ROOT_FS, 4, None, Some((20, Some(1020))))
            .dir("/home/me/repos", ROOT_FS, 500, Some("srv"), Some((50, Some(1050))))
            .dir("/home/me/repos/wt", ROOT_FS, 501, None, Some((50, Some(1050))))
    }

    /// `bound` with the bind mount taken down: its mount point, an empty directory of `/`'s.
    fn unbound(fs: Seen) -> Seen {
        fs.gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 600, Some("mountpoint"), Some((20, Some(1020))))
    }

    #[test]
    fn the_mount_a_checkout_is_on_is_its_highest_directory_on_that_mount() {
        let at = recorded(&bound(), "/home/me/repos/wt");
        let location = at.location.unwrap();
        assert_eq!(location.mount_root, Path::new("/home/me/repos"), "a bind mount of the same device is a mount of its own");
        assert_eq!((location.root, location.root_born.as_deref()), ([ROOT_FS, 500], Some("srv")));
        assert_eq!(location.mount, Some(MountInstance { id: 50, unique: Some(1050), boot: "a".into(), namespace: NS }));
        // Without the kernel saying which mount, nothing is recorded where mounts share devices.
        let blind = Seen::linux().dir("/", ROOT_FS, 2, None, None).dir("/wt", ROOT_FS, 5, None, None);
        assert_eq!(location_with(&blind, Path::new("/wt")), None);
        // With no boot or namespace to read the id in, the mount's root is still recorded.
        let mut unread = bound();
        unread.namespace = None;
        let location = location_with(&unread, Path::new("/home/me/repos/wt")).unwrap();
        assert_eq!((location.mount_root.as_path(), location.mount), (Path::new("/home/me/repos"), None));
        // macOS: by device alone.
        let mac = Seen::mac().dir("/", ROOT_FS, 2, None, None).dir("/Volumes", ROOT_FS, 3, None, None).dir("/Volumes/ext", VOLUME, 2, Some("x"), None).dir("/Volumes/ext/wt", VOLUME, 9, None, None);
        let location = location_with(&mac, Path::new("/Volumes/ext/wt")).unwrap();
        assert_eq!((location.mount_root.as_path(), location.root, location.mount), (Path::new("/Volumes/ext"), [VOLUME, 2], None));
    }

    #[test]
    fn a_checkout_is_gone_only_when_its_own_mount_is_there_to_say_so() {
        let fs = || bound();
        let at = recorded(&fs(), "/home/me/repos/wt");
        let wt = Path::new("/home/me/repos/wt");
        assert_eq!(presence_with(&fs(), wt, &at), Presence::Present);
        // Deleted from the mount, which is there: gone, and so is a tree deleted with it.
        assert_eq!(presence_with(&fs().gone("/home/me/repos/wt"), wt, &at), Presence::Absent);
        assert_eq!(presence_with(&fs().gone("/home/me/repos/wt"), Path::new("/home/me/repos/a/b"), &at), Presence::Absent);
        // A file where a directory above it was: nothing can be at the path.
        let filed = fs().gone("/home/me/repos/wt").file("/home/me/repos/x", ROOT_FS).failing("/home/me/repos/x/wt", libc::ENOTDIR);
        assert_eq!(presence_with(&filed, Path::new("/home/me/repos/x/wt"), &at), Presence::Absent);
        // A lookup refused or failing says nothing, whether of the path or of a directory above.
        assert!(unknown(presence_with(&fs().failing("/home/me/repos/wt", libc::EACCES), wt, &at)).contains("cannot be looked up"));
        assert!(unknown(presence_with(&fs().failing("/home/me/repos/wt", libc::EIO), wt, &at)).contains("cannot be looked up"));
        let refused = fs().gone("/home/me/repos/wt").failing("/home/me", libc::EACCES);
        assert!(unknown(presence_with(&refused, wt, &at)).contains("/home/me cannot be looked up"));
        assert!(unknown(presence_with(&Seen::linux(), wt, &at)).contains("no directory above it"));
    }

    /// The case the device alone got wrong: a bind mount of a directory of the same filesystem,
    /// taken down, leaves an empty directory of that filesystem where the checkout's mount was.
    #[test]
    fn a_bind_mount_of_the_same_filesystem_that_is_not_mounted_is_not_taken_for_a_deletion() {
        let at = recorded(&bound(), "/home/me/repos/wt");
        let why = unknown(presence_with(&unbound(bound()), Path::new("/home/me/repos/wt"), &at));
        assert!(why.contains("on another mount (rooted at /)") && why.contains("may not be there"), "{why}");
        // Its mount point holding something does not make it the mount either.
        let full = unbound(bound()).dir("/home/me/repos/other", ROOT_FS, 601, None, Some((20, Some(1020))));
        assert!(matches!(presence_with(&full, Path::new("/home/me/repos/wt"), &at), Presence::Unknown(_)));
        // Mounted again from the same directory, the checkout deleted from it: gone.
        let remounted = bound().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, Some("srv"), Some((51, Some(1051))));
        assert!(unknown(presence_with(&remounted, Path::new("/home/me/repos/wt"), &at)).contains("another mount id"));
        // ...in a later boot, where the root's birth time recognises the directory mounted.
        let later = Seen { boot: Some("b"), ..bound().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, Some("srv"), Some((51, Some(1051)))) };
        assert_eq!(presence_with(&later, Path::new("/home/me/repos/wt"), &at), Presence::Absent);
        // Not mounted in that later boot: still not a deletion.
        let later_down = Seen { boot: Some("b"), ..unbound(bound()) };
        assert!(matches!(presence_with(&later_down, Path::new("/home/me/repos/wt"), &at), Presence::Unknown(_)));
    }

    /// Mount ids are reused once a mount is gone, and mean nothing in another boot or mount
    /// namespace; the same inode on the same device at the same place can be another
    /// directory's after a mount was replaced.
    #[test]
    fn mount_ids_are_compared_only_where_they_mean_the_same_mount() {
        let wt = Path::new("/home/me/repos/wt");
        let at = recorded(&bound(), "/home/me/repos/wt");
        // A reused id under another unique id: another mount.
        let reused = bound().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, Some("srv"), Some((50, Some(1077))));
        assert!(unknown(presence_with(&reused, wt, &at)).contains("another mount id"));
        // No unique ids (before Linux 6.8): the reusable id, with the root's place and identity.
        let old_kernel = || {
            Seen::linux()
                .dir("/", ROOT_FS, 2, None, Some((20, None)))
                .dir("/home", ROOT_FS, 3, None, Some((20, None)))
                .dir("/home/me", ROOT_FS, 4, None, Some((20, None)))
                .dir("/home/me/repos", ROOT_FS, 500, None, Some((50, None)))
                .dir("/home/me/repos/wt", ROOT_FS, 501, None, Some((50, None)))
        };
        let old_at = recorded(&old_kernel(), "/home/me/repos/wt");
        assert_eq!(presence_with(&old_kernel().gone("/home/me/repos/wt"), wt, &old_at), Presence::Absent);
        let replaced = old_kernel().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, None, Some((51, None)));
        assert!(matches!(presence_with(&replaced, wt, &old_at), Presence::Unknown(_)));
        // Another boot or namespace, and no birth time to recognise the root by: unknown.
        for fs in [Seen { boot: Some("b"), ..old_kernel().gone("/home/me/repos/wt") }, Seen { namespace: Some(NS + 1), ..old_kernel().gone("/home/me/repos/wt") }, Seen { boot: None, ..old_kernel().gone("/home/me/repos/wt") }] {
            assert!(unknown(presence_with(&fs, wt, &old_at)).contains("another boot or mount namespace"));
        }
        // The same device and inode at the root's place, created at another time: another
        // directory (a filesystem made again, a tmpfs mounted again).
        let remade = bound().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, Some("later"), Some((50, Some(1050))));
        assert!(unknown(presence_with(&remade, wt, &at)).contains("created at another time"));
        // The same inode on another device: another filesystem.
        let other = bound().gone("/home/me/repos/wt").dir("/home/me/repos", VOLUME, 500, Some("srv"), Some((50, Some(1050))));
        assert!(unknown(presence_with(&other, wt, &at)).contains("on another filesystem"));
        // The mount the deepest directory is on cannot be told: unknown.
        let blind = bound().gone("/home/me/repos/wt").dir("/home/me/repos", ROOT_FS, 500, Some("srv"), None);
        assert!(unknown(presence_with(&blind, wt, &at)).contains("which mount"));
    }

    #[test]
    fn a_checkout_on_a_volume_that_is_not_mounted_is_not_taken_for_deleted() {
        // macOS: the volume's directory under /Volumes goes with it.
        let mac = || Seen::mac().dir("/", ROOT_FS, 2, None, None).dir("/Volumes", ROOT_FS, 3, None, None);
        let at = recorded(&mac().dir("/Volumes/ext", VOLUME, 2, Some("x"), None).dir("/Volumes/ext/repos", VOLUME, 5, None, None).dir("/Volumes/ext/repos/wt", VOLUME, 6, None, None), "/Volumes/ext/repos/wt");
        let why = unknown(presence_with(&mac(), Path::new("/Volumes/ext/repos/wt"), &at));
        assert!(why.contains("/Volumes is on another filesystem") && why.contains("may not be there"), "{why}");
        // Another volume given the same device number, mounted at the same place: its root was
        // created at another time.
        let other = mac().dir("/Volumes/ext", VOLUME, 2, Some("y"), None);
        assert!(unknown(presence_with(&other, Path::new("/Volumes/ext/repos/wt"), &at)).contains("created at another time"));
        // Mounted again, the checkout deleted from it: gone.
        let mounted = mac().dir("/Volumes/ext", VOLUME, 2, Some("x"), None).dir("/Volumes/ext/repos", VOLUME, 5, None, None);
        assert_eq!(presence_with(&mounted, Path::new("/Volumes/ext/repos/wt"), &at), Presence::Absent);
        // Linux: the mount point stays, empty, on the filesystem above it.
        let linux = || Seen::linux().dir("/", ROOT_FS, 2, None, Some((20, None))).dir("/mnt", ROOT_FS, 3, None, Some((20, None)));
        let at = recorded(&linux().dir("/mnt/ext", VOLUME, 2, None, Some((60, None))).dir("/mnt/ext/wt", VOLUME, 9, None, Some((60, None))), "/mnt/ext/wt");
        let empty = linux().dir("/mnt/ext", ROOT_FS, 40, None, Some((20, None)));
        assert!(unknown(presence_with(&empty, Path::new("/mnt/ext/wt"), &at)).contains("/mnt/ext is on another filesystem"));
        let mounted = linux().dir("/mnt/ext", VOLUME, 2, None, Some((60, None)));
        assert_eq!(presence_with(&mounted, Path::new("/mnt/ext/wt"), &at), Presence::Absent);
    }

    /// Records an earlier Stack kept: a device alone decides only where every mount has a
    /// device of its own; with nothing recorded, nothing decides, whatever the parent holds.
    #[test]
    fn a_record_without_a_location_is_gone_only_by_its_device_where_mounts_cannot_share_one() {
        let device_only = Recorded { device: Some(ROOT_FS), location: None };
        let nothing = Recorded::default();
        let mac = Seen::mac().dir("/", ROOT_FS, 2, None, None).dir("/Users", ROOT_FS, 3, None, None).dir("/Users/me", ROOT_FS, 4, None, None);
        assert_eq!(presence_with(&mac, Path::new("/Users/me/wt"), &device_only), Presence::Absent);
        assert!(unknown(presence_with(&mac, Path::new("/Users/me/wt"), &Recorded { device: Some(VOLUME), location: None })).contains("another filesystem"));
        let why = unknown(presence_with(&mac, Path::new("/Users/me/wt"), &nothing));
        assert!(why.contains("recorded no filesystem") && why.contains("recreate the directory"), "{why}");
        // Linux: the device of an unmounted same-filesystem bind mount's mount point is the same.
        let why = unknown(presence_with(&unbound(bound()), Path::new("/home/me/repos/wt"), &device_only));
        assert!(why.contains("only the device") && why.contains("bind mount"), "{why}");
        assert!(matches!(presence_with(&bound().gone("/home/me/repos/wt"), Path::new("/home/me/repos/wt"), &device_only), Presence::Unknown(_)));
        assert!(matches!(presence_with(&bound().gone("/home/me/repos/wt"), Path::new("/home/me/repos/wt"), &nothing), Presence::Unknown(_)));
        // Older records still read, and read back with nothing new.
        let legacy: Recorded = serde_json::from_str(r#"{"project_device": 7}"#).unwrap();
        assert_eq!(legacy, Recorded { device: Some(7), location: None });
        assert_eq!(serde_json::to_string(&legacy).unwrap(), r#"{"project_device":7}"#);
        // A location that cannot be learned now never drops the one recorded.
        let full = recorded(&bound(), "/home/me/repos/wt");
        assert_eq!(Recorded { device: Some(ROOT_FS), location: None }.or_kept(&full), full);
        assert_eq!(full.clone().or_kept(&Recorded::default()), full);
    }

    /// The same decisions on the disk: a refused lookup, a recorded mount that is not the one
    /// above the path, a plain deletion, and a legacy record completed while its checkout is there.
    #[test]
    fn on_disk_a_refused_lookup_is_unknown_and_a_deletion_is_gone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let wt = parent.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(parent.join("sibling"), b"").unwrap();
        let at = Recorded::of(&wt);
        let location = at.location.clone().expect("a temporary directory's mount can be learned");
        assert!(wt.canonicalize().unwrap().starts_with(&location.mount_root), "{location:?}");
        if crate::mounts::BINDS {
            assert!(location.mount.is_some(), "{location:?}");
        }
        assert_eq!(presence(&wt, &at), Presence::Present);
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
            let refused = presence(&wt, &at);
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(unknown(refused).contains("cannot be looked up"));
        }
        // A legacy record of it, completed while it is there.
        let mut legacy = Recorded { device: at.device, location: None };
        let mut refresh = Refresh::default();
        assert!(refresh.apply(&wt, &mut legacy));
        assert_eq!(legacy, at);
        assert!(!refresh.apply(&wt, &mut legacy), "nothing more to record");
        std::fs::remove_dir(&wt).unwrap();
        assert_eq!(presence(&wt, &at), Presence::Absent);
        assert!(gone(&wt, &at));
        // Nothing to complete once it is gone.
        let mut missing = Recorded { device: at.device, location: None };
        assert!(!Refresh::default().apply(&wt, &mut missing));
        // A record of another mount's root at that place: not this one.
        let elsewhere = Recorded { location: Some(Location { root: [location.root[0], location.root[1].wrapping_add(1)], ..location.clone() }), ..at.clone() };
        assert!(unknown(presence(&wt, &elsewhere)).contains("on another mount"));
        let other_device = Recorded { location: Some(Location { root: [location.root[0].wrapping_add(1), location.root[1]], ..location }), ..at };
        assert!(unknown(presence(&wt, &other_device)).contains("on another filesystem"));
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
