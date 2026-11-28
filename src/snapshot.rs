//! Snapshots of service data directories, shared by every checkout of a project.
//!
//! mise's Postgres and Redis presets keep each checkout's data in a directory under mise's
//! state, which `mise daemons --json` reports, before the first start included. Neither server
//! stores that path inside the directory: Postgres gets it from `-D` and Redis from `--dir` on
//! the command line, and Stack's instance check compares what the server reports against what
//! the supervisor reports for this checkout. A copy placed at another checkout's path therefore
//! verifies as that checkout's instance. Identity tokens of probed services live in Stack's
//! machine state, keyed by checkout, never in a data directory; only Postgres and Redis can be
//! snapshotted.
//!
//! The data directory is the one `up` recorded for this checkout (see [`crate::datadirs`]): the
//! supervisor must name the recorded directory, the directory must still be the one recorded
//! (reached by its recorded location with no link followed, the same device and inode) and
//! carry this checkout's marker. A restore replaces it with another directory, so the record is
//! bound to the new one at once.
//!
//! A data directory is copied only while its service is confirmed stopped (see
//! [`session::with_stopped`]); copies use copy-on-write where the filesystem offers it, and a
//! restore replaces the directory with one rename (an exchange where the platform has one),
//! never by copying into it.

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use crate::datadirs::{self, Found};
use crate::init_tasks::{self, DirIdentity, InitState, Marker};
use crate::project::{self, Options, Report};
use crate::provider::mise::DaemonStatus;
use crate::session::{self, Ctx, UpOptions};
use crate::source::Mode;
use crate::state::{now, write_json, FileLock};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Presets whose data directory Stack can copy, and whose instance check is the directory.
const SUPPORTED: &[&str] = &["postgres", "redis"];

/// Files a stopped server leaves that describe one run of one process, not the data.
const RUN_FILES: &[&str] = &["postmaster.pid", "postmaster.opts"];

const META: &str = "meta.json";

// ---- project identity ---------------------------------------------------------------------

/// Which checkouts share snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectId {
    pub id: String,
    /// `git`: checkouts of one repository (worktrees included) at the same subdirectory.
    /// `checkout`: this directory only (not in a Git repository).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
}

/// The Git common directory (shared by a repository and all its worktrees) and the project's
/// path inside its worktree; the checkout itself outside Git.
pub fn project_id(root: &Path) -> ProjectId {
    let git = || -> Option<(PathBuf, String)> {
        let mut command = std::process::Command::new("git");
        command
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel"])
            .env("GIT_TERMINAL_PROMPT", "0");
        let out = crate::process::capture(&mut command, std::time::Duration::from_secs(10), 64 * 1024).ok()?;
        if out.exit_code != Some(0) || out.timed_out {
            return None;
        }
        let mut lines = out.stdout.lines();
        let common = fs::canonicalize(lines.next()?.trim()).ok()?;
        let top = fs::canonicalize(lines.next()?.trim()).ok()?;
        let sub = root.strip_prefix(&top).ok()?.to_string_lossy().into_owned();
        Some((common, sub))
    };
    match git() {
        Some((common, sub)) => ProjectId::git(common, sub),
        None => ProjectId::checkout(root),
    }
}

/// [`project_id`] read from the checkout's Git files instead of asking `git` (see
/// [`crate::gitdir`]): the same identity for every repository Git itself lays out, at the cost
/// of a few file reads, so `ps` can name the project of every checkout it lists.
pub fn project_id_offline(root: &Path) -> ProjectId {
    project_id_of(root, crate::gitdir::locate(root).as_ref())
}

/// [`project_id_offline`] from Git directories already located for `root`.
pub fn project_id_of(root: &Path, dirs: Option<&crate::gitdir::GitDirs>) -> ProjectId {
    let git = dirs.and_then(|d| Some((d.common_dir.clone(), root.strip_prefix(&d.toplevel).ok()?.to_string_lossy().into_owned())));
    match git {
        Some((common, sub)) => ProjectId::git(common, sub),
        None => ProjectId::checkout(root),
    }
}

impl ProjectId {
    fn git(common: PathBuf, sub: String) -> Self {
        ProjectId {
            id: sha256_hex(format!("git\0{}\0{sub}", common.display()).as_bytes())[..16].to_string(),
            kind: "git",
            git_common_dir: Some(common),
            subdir: Some(sub),
            checkout: None,
        }
    }

    fn checkout(root: &Path) -> Self {
        ProjectId {
            id: sha256_hex(format!("checkout\0{}", root.display()).as_bytes())[..16].to_string(),
            kind: "checkout",
            git_common_dir: None,
            subdir: None,
            checkout: Some(root.to_path_buf()),
        }
    }
}

fn store(state: &Path, project: &ProjectId) -> PathBuf {
    state.join("snapshots").join(&project.id)
}

fn store_lock(state: &Path, project: &ProjectId) -> Result<FileLock> {
    FileLock::acquire(&state.join("snapshots").join(format!("{}.lock", project.id)))
}

// ---- the store's generations ----------------------------------------------------------------
//
// A snapshot is the directory `<store>/<name>` holding `meta.json` (written last, so its presence
// means the data beside it is complete) and `data/`. Every write happens under the store lock
// in a staging directory of the store, then is published by renames:
//
// - `.tmp-<name>-<pid>-<ns>`: a snapshot being built by `save`;
// - `.old-<name>-<pid>-<ns>`: a published generation set aside by `save --force` where the
//   filesystem has no exchange, until the new one is in place;
// - `.rm-<name>-<pid>-<ns>`: a snapshot `rm` took out of the listing, being deleted.
//
// `save --force` exchanges the new generation with the published one in one step
// (`renameatx_np(RENAME_SWAP)`, `renameat2(RENAME_EXCHANGE)`), so the name always holds a
// complete snapshot. Without an exchange it renames the published one aside first: a crash
// between the two renames leaves the name empty and the old generation in `.old-*`. Readers
// that take no lock (`list`, the checks before an operation) then read that old generation
// in place, and the next operation under the lock moves it back ([`recover_store`]) before it
// removes any staging directory. A save that is not interrupted moves the published one aside,
// publishes the new one and removes the old one between any two reads of a reader that takes
// no lock, so a reader that found neither looks again (see [`lookup`]) and keeps what it read
// rather than reading a directory twice. A generation counts only when its `meta.json` parses and
// names it: one at the name that does not is no generation, and never hides, or has removed,
// a valid one set aside. A read or rename that fails stops the operation with what was set
// aside left in place.

/// A staging directory of the store, by its name.
#[derive(Debug, PartialEq, Eq)]
struct Staging {
    kind: &'static str,
    name: String,
    pid: u32,
    nanos: u128,
}

impl Staging {
    fn parse(entry: &str) -> Option<Staging> {
        let (kind, rest) = [("tmp", ".tmp-"), ("old", ".old-"), ("rm", ".rm-")].into_iter().find_map(|(k, p)| entry.strip_prefix(p).map(|r| (k, r)))?;
        let mut parts = rest.rsplitn(3, '-');
        let nanos = parts.next()?.parse().ok()?;
        let pid = parts.next()?.parse().ok()?;
        let name = parts.next()?;
        valid_name(name).ok()?;
        Some(Staging { kind, name: name.to_string(), pid, nanos })
    }

    /// Whether another process that is still alive owns it. Every writer holds the store lock
    /// while it stages, so under the lock a staging directory is stale; one whose owner still
    /// runs is left alone all the same (a pid reused since only keeps it longer).
    fn live_elsewhere(&self) -> bool {
        self.pid != std::process::id() && crate::state::pid_alive(self.pid)
    }
}

/// `meta.json` of the snapshot directory `dir`, with that directory held open: `Some` when it
/// parses and names `name`, `None` when there is none or it is not a generation of `name`
/// (metadata that does not parse, or describes another snapshot, or a link where the directory
/// or its metadata should be). An error when it cannot be read: a generation is never judged
/// invalid, or valid, on a read that failed.
///
/// The metadata is read through the directory held, never by its path again, so it is that
/// directory's even when a save replaces what is at `dir` meanwhile; and while it is held, no
/// other directory can be given its device and inode (see [`Generation`]).
fn meta_at(dir: &Path, name: &str) -> Result<Option<(Meta, fs::File)>> {
    use std::io::{ErrorKind, Read};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    let quiet = |e: &std::io::Error| {
        matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory | ErrorKind::IsADirectory | ErrorKind::InvalidData) || e.raw_os_error() == Some(libc::ELOOP)
    };
    let held = match fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(dir) {
        Ok(held) => held,
        Err(e) if quiet(&e) => return Ok(None),
        Err(e) => return Err(io_error(dir.display(), e)),
    };
    let path = dir.join(META);
    let text = (|| {
        let file = std::ffi::CString::new(META).expect("META has no NUL");
        // SAFETY: openat on a descriptor held here, with a NUL-terminated name; the descriptor
        // returned is owned here.
        let fd = unsafe { libc::openat(held.as_raw_fd(), file.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a descriptor openat just returned, owned by nothing else.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        let mut text = String::new();
        file.read_to_string(&mut text).map(|_| text)
    })();
    match text {
        Ok(text) => Ok(serde_json::from_str::<Meta>(&text).ok().filter(|m| m.name == name).map(|m| (m, held))),
        Err(e) if quiet(&e) => Ok(None),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

/// The store's staging directories, by name.
fn staging(base: &Path) -> Result<Vec<(PathBuf, Staging)>> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(base.display(), e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_error(base.display(), e))?;
        if let Some(s) = entry.file_name().to_str().and_then(Staging::parse) {
            out.push((entry.path(), s));
        }
    }
    Ok(out)
}

/// The newest complete generation of `name` set aside by an interrupted `save --force`.
fn set_aside(base: &Path, name: &str) -> Result<Option<PathBuf>> {
    Ok(set_aside_read(base, name)?.0.map(|(path, ..)| path))
}

/// A generation's directory, with the metadata read there and the directory held open.
type Located = (PathBuf, Meta, fs::File);

/// [`set_aside`] with the metadata it read, and the staging directories of `name` it saw
/// (valid or not), in name order.
fn set_aside_read(base: &Path, name: &str) -> Result<(Option<Located>, Vec<PathBuf>)> {
    let mut old: Vec<(u64, u128, Located)> = Vec::new();
    let mut seen = Vec::new();
    for (path, s) in staging(base)? {
        if s.kind == "old" && s.name == name {
            seen.push(path.clone());
            if let Some((meta, held)) = meta_at(&path, name)? {
                old.push((meta.created_at, s.nanos, (path, meta, held)));
            }
        }
    }
    old.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    seen.sort();
    Ok((old.pop().map(|(_, _, located)| located), seen))
}

/// How many times a reader without the lock looks for a generation while what it sees keeps
/// changing (see [`lookup`]).
const LOOKUPS: usize = 100;

/// Where `name`'s snapshot is: the published directory when its metadata is valid, else,
/// while a `save --force` without an exchange has it renamed aside (or crashed with it so),
/// the newest valid generation set aside. Read only (see [`lookup`]).
fn generation(state: &Path, project: &ProjectId, name: &str) -> Result<Option<PathBuf>> {
    Ok(lookup(&store(state, project), name)?.map(|(path, ..)| path))
}

/// [`generation`] with the metadata it read there, which is what callers use: the directory is
/// never read a second time, since a save may remove it in between. Without the lock, a save
/// that moves the published generation aside after the name is read, then publishes the new
/// one and removes the old one before the generation set aside is read, leaves this reader
/// with neither; the new generation is in place by then, so the name is read again. Only
/// another save in the same few reads can hide it again, so this goes round while the
/// generations set aside it saw change, at most [`LOOKUPS`] times, and finds none only when two
/// rounds in a row saw the same: no generation at the name and none valid set aside.
fn lookup(base: &Path, name: &str) -> Result<Option<Located>> {
    let published = base.join(name);
    let mut before: Option<Vec<PathBuf>> = None;
    for _ in 0..LOOKUPS {
        if let Some((meta, held)) = meta_at(&published, name)? {
            return Ok(Some((published, meta, held)));
        }
        let (aside, seen) = set_aside_read(base, name)?;
        if aside.is_some() {
            return Ok(aside);
        }
        #[cfg(test)]
        BETWEEN_READS.with(|hook| {
            if let Some(f) = hook.borrow_mut().as_mut() {
                f();
            }
        });
        if before.as_ref() == Some(&seen) {
            break;
        }
        before = Some(seen);
    }
    Ok(meta_at(&published, name)?.map(|(meta, held)| (published, meta, held)))
}

#[cfg(test)]
thread_local! {
    /// Tests run this in [`lookup`] after a round found nothing, as a save would act there.
    static BETWEEN_READS: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

/// Rename within the store; tests fail a named step, as an I/O error would.
fn store_rename(from: &Path, to: &Path, step: &'static str) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_RENAMES.with(std::cell::Cell::get).contains(&step) {
        return Err(std::io::Error::other(format!("{step} failed (test)")));
    }
    let _ = step;
    fs::rename(from, to)
}

/// Under the store lock: put back every generation an interrupted `save --force` left set
/// aside with no valid one published in its place, then remove staging directories nobody owns
/// any more. Returns what it did, for the report. Whatever cannot be read or moved is an error,
/// with the generation set aside left where it is for the next operation; one set aside is
/// never removed while the name holds no valid generation.
fn recover_store(base: &Path) -> Result<Vec<Value>> {
    let mut done = Vec::new();
    let names: std::collections::BTreeSet<String> = staging(base)?.into_iter().filter(|(_, s)| s.kind == "old").map(|(_, s)| s.name).collect();
    for name in names {
        let published = base.join(&name);
        if meta_at(&published, &name)?.is_some() {
            continue;
        }
        let Some(old) = set_aside(base, &name)? else { continue };
        let failed = |e: StackError| {
            StackError::new("snapshot_failed", format!("snapshot '{name}' cannot be put back: {}; its last complete generation is kept at {}", e.message, old.display()))
                .hint("retry; the next snapshot command puts it back")
        };
        // What is at the name is no valid generation (a crash before its meta.json, or metadata
        // that does not parse or names another snapshot): staging too.
        match fs::symlink_metadata(&published) {
            Ok(_) => {
                let stray = unique(base, &format!(".rm-{name}"));
                store_rename(&published, &stray, "recover_stray").map_err(|e| failed(io_error(published.display(), e)))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(io_error(published.display(), e))),
        }
        store_rename(&old, &published, "recover_old").map_err(|e| failed(io_error(old.display(), e)))?;
        done.push(json!({ "snapshot": name, "recovered": "the generation an interrupted `save --force` set aside" }));
    }
    for (path, s) in staging(base)? {
        if s.live_elsewhere() {
            continue;
        }
        if s.kind == "old" && meta_at(&base.join(&s.name), &s.name)?.is_none() && meta_at(&path, &s.name)?.is_some() {
            continue;
        }
        remove_tree(&path);
    }
    Ok(done)
}

/// Under the store lock: publish the complete snapshot `tmp` at `published`, replacing what
/// is there. One exchange where the filesystem has one; otherwise the old generation is
/// renamed aside first, and put back if the new one cannot take its place (where even that
/// fails, it stays set aside, read in place and put back by the next operation).
fn publish(base: &Path, name: &str, tmp: &Path, published: &Path) -> Result<&'static str> {
    if fs::symlink_metadata(published).is_err() {
        store_rename(tmp, published, "publish").map_err(|e| io_error(published.display(), e))?;
        return Ok("rename");
    }
    if exchange(tmp, published).map_err(|e| io_error(published.display(), e))? {
        // `tmp` now holds the old generation.
        remove_tree(tmp);
        return Ok("exchange");
    }
    let old = unique(base, &format!(".old-{name}"));
    store_rename(published, &old, "aside").map_err(|e| io_error(published.display(), e))?;
    if let Err(e) = store_rename(tmp, published, "publish") {
        let e = io_error(published.display(), e);
        return Err(match store_rename(&old, published, "put_back") {
            Ok(()) => e,
            Err(back) => StackError::new("snapshot_failed", format!("{}; the generation it replaces is kept at {} ({back}), and the next snapshot command puts it back", e.message, old.display())),
        });
    }
    remove_tree(&old);
    Ok("rename")
}

#[cfg(test)]
thread_local! {
    /// Tests fail these steps of the store's renames (see [`store_rename`]).
    static FAIL_RENAMES: std::cell::Cell<&'static [&'static str]> = const { std::cell::Cell::new(&[]) };
}

pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(StackError::new("usage", format!("invalid snapshot name {name:?}"))
            .hint("use 1-64 letters, digits, '.', '_' or '-', starting with a letter or digit"))
    }
}

// ---- metadata -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapService {
    pub preset: String,
    /// The exact release stack.lock pinned when it was saved.
    pub version: String,
    pub major: String,
    pub files: u64,
    pub bytes: u64,
    /// Init state of the data when saved.
    pub init: InitState,
    /// Digest of the init list and tasks this data satisfies, when init had completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_digest: Option<String>,
    /// Where init stopped, when a task had failed: what `up` resumes from after a restore.
    /// A field of its own, so a Stack that does not know it restores such data as `pending`
    /// (init runs from the start), never as `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_resume: Option<Resume>,
}

/// The progress of an init that failed, as its marker recorded it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    /// Digest of the init list it ran.
    pub digest: String,
    /// The leading tasks it completed, each with its definition's digest.
    pub completed: Vec<init_tasks::CompletedTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    pub format: u32,
    pub project: String,
    pub created_at: u64,
    /// The checkout it was saved from.
    pub source: PathBuf,
    pub services: IndexMap<String, SnapService>,
}

/// One generation of a snapshot, as read: where it is, its metadata (read through the directory
/// held), and which directory it is (device, inode), which a rename keeps and a new `save`
/// changes.
///
/// The directory stays open for as long as this is kept, so its inode cannot be freed and given
/// to another directory: a `save --force` that replaces it and removes it, followed by another
/// save, could otherwise publish a new generation on the very inode, saved in the same second with
/// the same services, and pass for it. A restore keeps the generation it chose from before it
/// stops the services until it has compared it with the one read under the store lock.
struct Generation {
    dir: PathBuf,
    meta: Meta,
    identity: [u64; 2],
    _held: fs::File,
}

impl Generation {
    fn new((dir, meta, held): Located) -> Result<Generation> {
        use std::os::unix::fs::MetadataExt;
        let st = held.metadata().map_err(|e| io_error(dir.display(), e))?;
        Ok(Generation { identity: [st.dev(), st.ino()], dir, meta, _held: held })
    }

    /// The same generation: the very directory, which no other can be while `self` holds it
    /// open (and so saved at the same time, of the same services). Put back from where an
    /// interrupted `save --force` set it aside counts, a new save does not.
    fn same(&self, other: &Generation) -> bool {
        self.identity == other.identity && self.meta.created_at == other.meta.created_at && self.meta.services.keys().eq(other.meta.services.keys())
    }
}

/// `name`'s snapshot (see [`generation`]), whose metadata must name it.
fn read_generation(state: &Path, project: &ProjectId, name: &str) -> Result<Generation> {
    if let Some(found) = lookup(&store(state, project), name)? {
        return Generation::new(found);
    }
    // No generation: why, from what is at the name.
    let dir = store(state, project).join(name);
    let path = dir.join(META);
    let invalid = |why: String| StackError::new("snapshot_invalid", format!("{}: {why}", path.display())).hint(format!("remove it with `stack snapshot rm {name}`"));
    match fs::read_to_string(&path) {
        Ok(text) => {
            let meta: Meta = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
            if meta.name != name {
                return Err(invalid(format!("it describes snapshot '{}'", meta.name)));
            }
            // Published since the lookup: read again, through the directory held.
            match meta_at(&dir, name)? {
                Some((meta, held)) => Generation::new((dir, meta, held)),
                None => Err(invalid("it is reached through a link, or changed while it was read".into())),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_found(state, project, name)),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

/// The metadata of `name`'s snapshot (see [`read_generation`]).
fn read_meta(state: &Path, project: &ProjectId, name: &str) -> Result<Meta> {
    read_generation(state, project, name).map(|g| g.meta)
}

fn not_found(state: &Path, project: &ProjectId, name: &str) -> StackError {
    let known = names(state, project);
    StackError::new("snapshot_not_found", format!("no snapshot named '{name}' for this project")).hint(if known.is_empty() {
        "save one with `stack snapshot save <name>` in a checkout whose database is ready".to_string()
    } else {
        format!("snapshots: {} (`stack snapshot list`)", known.join(", "))
    })
}

/// Every snapshot's name: published ones, and those an interrupted `save --force` left set
/// aside with nothing published in their place.
///
/// Read without the lock, so the published names are listed again after the generations set
/// aside: a save may move one aside after the first listing and remove it before the second
/// (see [`lookup`]). Rounds repeat while the names found change, at most [`LOOKUPS`] times.
fn names(state: &Path, project: &ProjectId) -> Vec<String> {
    let base = store(state, project);
    let published = || -> Vec<String> {
        fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(META).is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_name(n).is_ok())
            .collect()
    };
    let mut before: Option<Vec<String>> = None;
    for _ in 0..LOOKUPS {
        let mut out = published();
        for (path, s) in staging(&base).unwrap_or_default() {
            if s.kind == "old" && meta_at(&path, &s.name).ok().flatten().is_some() {
                out.push(s.name);
            }
        }
        out.extend(published());
        out.sort();
        out.dedup();
        if before.as_ref() == Some(&out) {
            break;
        }
        before = Some(out);
    }
    before.unwrap_or_default()
}

fn major(version: &str) -> String {
    version.split(['.', '-', '+']).next().unwrap_or(version).to_string()
}

/// The exact release stack.lock pins for a preset service.
fn locked_version(report: &Report, service: &str) -> Option<String> {
    report.versions.iter().find(|v| v.kind == "service" && v.name == service).and_then(|v| v.resolved.clone())
}

fn preset<'a>(report: &'a Report, service: &str) -> Option<&'a str> {
    report.stack.services.get(service).and_then(|e| e.value.preset.as_deref())
}

/// Every snapshotted service exists here with the same preset and major version.
fn compatible(meta: &Meta, report: &Report) -> Result<()> {
    for (service, snap) in &meta.services {
        let here = preset(report, service);
        if here != Some(snap.preset.as_str()) {
            return Err(StackError::new(
                "snapshot_incompatible",
                format!("snapshot '{}' holds {} data for service '{service}', but this checkout's '{service}' is {}", meta.name, snap.preset, here.map_or("not defined".to_string(), |p| format!("a {p} service"))),
            )
            .hint("restore it into a checkout whose stack.toml defines the same service, or save a new snapshot")
            .with_detail(json!({ "service": service, "snapshot": snap.preset, "checkout": here })));
        }
        let version = locked_version(report, service).unwrap_or_default();
        if major(&version) != snap.major {
            return Err(StackError::new(
                "snapshot_incompatible",
                format!("snapshot '{}' holds {} {} data for '{service}', but stack.lock pins {version}; a different major version cannot read it", meta.name, snap.preset, snap.version),
            )
            .hint("pin the snapshot's major version, or save a new snapshot from a checkout on this version")
            .with_detail(json!({ "service": service, "snapshot": snap.version, "locked": version })));
        }
    }
    Ok(())
}

/// A service's data directory, as the supervisor names it for this checkout and `up` recorded
/// it (see [`crate::datadirs`]).
struct DataDir {
    /// As the supervisor names it, and the record with it.
    dir: PathBuf,
    /// Where it resolves, with every link resolved: where data is copied from and to.
    location: PathBuf,
    /// The recorded directory is there (`false`: nothing is there yet).
    exists: bool,
    /// This checkout's token, which the record and the marker beside the directory name.
    token: String,
    /// The recorded directory, held open as checked; `None` when nothing is there yet.
    held: Option<datadirs::Held>,
}

/// The data directory of this checkout's `service`: the one the supervisor names, recorded by
/// `up` for this very checkout, and, when it exists, still that directory with this checkout's
/// marker beside it. Anything else is refused, never copied from or replaced.
fn data_dir(ctx: &Ctx, statuses: &[DaemonStatus], service: &str) -> Result<DataDir> {
    let failed = |message: String| StackError::new("snapshot_failed", message);
    let rerun = "run `stack up` once so Stack records this checkout's data directory, then retry";
    let dir = statuses.iter().find(|d| d.name == service).and_then(|d| d.data_dir.as_deref()).map(PathBuf::from).ok_or_else(|| {
        failed(format!("the supervisor reports no data directory for '{service}'")).hint("run `stack up` once so the service is configured, then retry")
    })?;
    let token = crate::checkout::token(&ctx.root).ok_or_else(|| failed(format!("cannot tell which directory {} is", ctx.root.display())))?;
    let entry = datadirs::entries(&ctx.state)?
        .into_iter()
        .find(|e| e.project == ctx.root && e.service == service && e.dir == dir && e.checkout == token)
        .ok_or_else(|| failed(format!("Stack has no record of {} as {service}'s data directory for this checkout", dir.display())).hint(rerun))?;
    let marked = |bytes: Option<Vec<u8>>| bytes.as_deref().and_then(session::marker_owner).as_deref() == Some(token.as_str());
    let absent = matches!(fs::symlink_metadata(&dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    match datadirs::hold(&entry) {
        Found::Held(held) => {
            if !marked(held.beside(session::MARKER_SUFFIX)) {
                return Err(failed(format!("{} does not carry this checkout's marker", dir.display())).hint(rerun));
            }
            let location = entry.location.clone().expect("a held record has a location");
            Ok(DataDir { dir, location, exists: true, token, held: Some(held) })
        }
        // Nothing there yet: it may be created, at the place the record says.
        Found::Gone | Found::Unbound(_) if absent => {
            let location = datadirs::locate(&dir).0.filter(|l| entry.location.as_ref().is_none_or(|r| r == l)).ok_or_else(|| {
                failed(format!("{} no longer resolves where Stack recorded it", dir.display())).hint(rerun)
            })?;
            if !marked(fs::read(session::data_marker(&dir)).ok()) {
                return Err(failed(format!("{} does not carry this checkout's marker", dir.display())).hint(rerun));
            }
            Ok(DataDir { dir, location, exists: false, token, held: None })
        }
        Found::Gone | Found::Unbound(_) => Err(failed(format!("{} is not the data directory Stack recorded for '{service}'", dir.display())).hint(rerun)),
        Found::Other(why) => Err(StackError::new("snapshot_unsupported", format!("{service}'s data directory: {why}")).hint("Stack copies only the data directory it recorded, reached with no link followed")),
    }
}

// ---- copying ------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    files: u64,
    bytes: u64,
}

/// Every entry under `dir` must be a directory or a regular file: a symbolic link (such as a
/// Postgres tablespace in `pg_tblspc`) would point outside it, and is refused.
fn check_tree(dir: &Path) -> Result<Tally> {
    let mut tally = Tally::default();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).map_err(|e| io_error(d.display(), e))? {
            let entry = entry.map_err(|e| io_error(d.display(), e))?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| io_error(path.display(), e))?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                tally.files += 1;
                tally.bytes += meta.len();
            } else {
                return Err(StackError::new(
                    "snapshot_unsupported",
                    format!("{} is not a regular file or directory (a symbolic link, socket or device); Stack copies only self-contained data directories", path.display()),
                )
                .hint("tablespaces and links outside the data directory cannot be snapshotted"));
            }
        }
    }
    Ok(tally)
}

/// Copy `src` to `dst` (which must not exist), dropping `RUN_FILES` at its top. Modes are
/// kept (Postgres refuses a data directory others can read). Copy-on-write where available:
/// `clonefile` on macOS (APFS), and per file whatever `std::fs::copy` uses (`fclonefileat`
/// on macOS, `copy_file_range` on Linux, which reflinks on Btrfs and XFS).
fn copy_tree(src: &Path, dst: &Path) -> Result<&'static str> {
    check_tree(src)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        const CLONE_NOFOLLOW: u32 = 0x0001;
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).ok();
        if let (Some(s), Some(d)) = (c(src), c(dst)) {
            // SAFETY: both are valid NUL-terminated paths; clonefile only reads them.
            if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) } == 0 {
                for name in RUN_FILES {
                    let _ = fs::remove_file(dst.join(name));
                }
                // A clone is of what was there; check it again, as a plain copy would.
                check_tree(dst)?;
                return Ok("clonefile");
            }
        }
    }
    copy_dir(src, dst, true)?;
    Ok("copy")
}

fn copy_dir(src: &Path, dst: &Path, top: bool) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(|e| io_error(src.display(), e))?;
    fs::create_dir(dst).map_err(|e| io_error(dst.display(), e))?;
    for entry in fs::read_dir(src).map_err(|e| io_error(src.display(), e))? {
        let entry = entry.map_err(|e| io_error(src.display(), e))?;
        let name = entry.file_name();
        if top && RUN_FILES.iter().any(|f| name == *f) {
            continue;
        }
        let (from, to) = (entry.path(), dst.join(&name));
        let m = fs::symlink_metadata(&from).map_err(|e| io_error(from.display(), e))?;
        if m.is_dir() {
            copy_dir(&from, &to, false)?;
        } else if m.is_file() {
            fs::copy(&from, &to).map_err(|e| io_error(from.display(), e))?;
        } else {
            return Err(StackError::new("snapshot_unsupported", format!("{} is not a regular file or directory", from.display())));
        }
    }
    fs::set_permissions(dst, meta.permissions()).map_err(|e| io_error(dst.display(), e))?;
    Ok(())
}

/// A fresh name in `parent`: `<prefix>-<pid>-<nanoseconds since the epoch>`. The pid says which
/// process owns it (see [`Staging`]), the time orders generations.
fn unique(parent: &Path, prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    parent.join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn remove_tree(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Exchange two existing paths in one step (see [`datadirs::exchange`]).
fn exchange(a: &Path, b: &Path) -> std::io::Result<bool> {
    datadirs::exchange(a, b)
}

// ---- restoring ----------------------------------------------------------------------------
//
// A restore of several services is one change: every service's data is replaced, or none is.
//
// 1. Every service's snapshot data is copied beside its data directory
//    (`.<dir>.stack-restore-…`) before anything is replaced. A failure removes the copies.
// 2. A journal (`<state>/init/<checkout>.restore.json`) records each swap: the data
//    directory, its copy, where its old data goes, and which directories they are (device,
//    inode), and the init markers to write once every copy is in place.
// 3. Each data directory, held open and checked as `up` recorded it (see [`datadirs::hold`]),
//    on its parent's mount, is swapped with its copy through the held parent: one exchange
//    where the filesystem has it, else two renames (`.<dir>.stack-old-…`). The old data is
//    kept until step 4.
// 4. The journal is marked `committed` (the restore has happened). Once the checkout is shown
//    to be the one that restored (its token), each copy to be at its place as the directory
//    the restore made, and each record to still name this checkout's directory there, the init
//    markers are written in one write, each record is bound to its copy by the identity
//    journaled, the old data is removed as `gc --reclaim-data` removes data, and the journal
//    goes. Anything else keeps the journal and the old data (`restore_incomplete`).
//
// A failure in step 3 swaps every service back, judged by which directory is where, never by
// what the steps meant to do: services not reached are `unchanged`, those swapped are
// `rolled_back`. One that cannot be put back keeps its old data where it is, the journal stays,
// nothing is recorded as restored, and the error (`restore_incomplete`) says where each
// service's data is. An interrupted restore (a crash) is finished the same way by the next
// snapshot operation or `up` of the checkout, before anything else touches the data: rolled
// back before step 4, completed after it, and only with every service the journal names
// confirmed stopped. `restart` refuses while a journal is there (see [`refuse_pending`]).

/// One service's part of a restore, as the journal records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Swap {
    service: String,
    /// As the supervisor names it, and the record with it.
    dir: PathBuf,
    /// Where it resolves: what is swapped.
    target: PathBuf,
    /// The complete copy of the snapshot's data, beside `target`. Exchanged, the old data.
    fresh: PathBuf,
    /// Where the old data is renamed to where the filesystem has no exchange.
    aside: PathBuf,
    /// The data directory before the restore (device, inode); `None`: nothing was there.
    old: Option<[u64; 2]>,
    /// The copy (device, inode).
    new: [u64; 2],
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    snapshot: String,
    /// `swapping` until every copy is in place, then `committed`.
    phase: String,
    token: String,
    swaps: Vec<Swap>,
    /// The init marker of each service once committed (`None` removes it).
    markers: IndexMap<String, Option<Marker>>,
}

fn journal_path(state: &Path, root: &Path) -> PathBuf {
    state.join("init").join(format!("{}.restore.json", crate::state::project_key(root)))
}

/// The directory at `path` (device, inode), not following a link; `None` when nothing, or no
/// directory, is there.
fn dir_at(path: &Path) -> Option<[u64; 2]> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).ok().filter(|m| m.is_dir()).map(|m| [m.dev(), m.ino()])
}

/// Remove the directory at `path` if it is `identity`: a copy this restore made, never
/// anything else (checked on the directory held open, reached with no link followed). Never
/// across a mount inside it.
fn discard(path: &Path, identity: [u64; 2]) {
    let _ = datadirs::remove_tree(path, identity);
}

fn rename(from: &Path, to: &Path) -> std::result::Result<(), String> {
    fs::rename(from, to).map_err(|e| format!("renaming {} to {}: {e}", from.display(), to.display()))
}

/// Put `swap`'s old data back at its place, whatever step its swap reached, and remove the
/// copy. `unchanged` when it was never swapped, `rolled_back` when it was swapped back; why
/// not when neither can be shown, with everything left where it is.
fn undo(swap: &Swap) -> std::result::Result<&'static str, String> {
    let (target, fresh, aside) = (dir_at(&swap.target), dir_at(&swap.fresh), dir_at(&swap.aside));
    let lost = || {
        format!(
            "{} is neither its old data nor the restored copy; the old data was {}",
            swap.target.display(),
            match (fresh, aside) {
                (f, _) if f.is_some() && f == swap.old => format!("left at {}", swap.fresh.display()),
                (_, a) if a.is_some() && a == swap.old => format!("left at {}", swap.aside.display()),
                _ => "not found".to_string(),
            }
        )
    };
    let Some(old) = swap.old else {
        // Nothing was there: the copy was renamed in, or not (whatever is there now, if the
        // copy never moved, is not the restore's).
        return match target {
            _ if fresh == Some(swap.new) => {
                discard(&swap.fresh, swap.new);
                Ok("unchanged")
            }
            None => Ok("unchanged"),
            Some(t) if t == swap.new && fresh.is_none() => {
                rename(&swap.target, &swap.fresh)?;
                discard(&swap.fresh, swap.new);
                Ok("rolled_back")
            }
            Some(_) => Err(format!("{} is not the restored copy; it is left as it is", swap.target.display())),
        };
    };
    if target == Some(old) {
        discard(&swap.fresh, swap.new);
        return Ok("unchanged");
    }
    if target.is_none() && aside == Some(old) {
        // Renamed aside, the copy not in place yet.
        rename(&swap.aside, &swap.target)?;
    } else if fresh == Some(swap.new) && aside != Some(old) {
        // The copy never moved, so neither did anything else this restore touches: whatever
        // is at the place now (another directory put there since the check) is left alone.
        discard(&swap.fresh, swap.new);
        return Ok("unchanged");
    } else if target == Some(swap.new) && fresh == Some(old) {
        // Exchanged: exchange back (two renames through `aside` without an exchange now).
        if !exchange(&swap.fresh, &swap.target).map_err(|e| format!("exchanging {} back: {e}", swap.target.display()))? {
            rename(&swap.target, &swap.aside)?;
            if let Err(e) = rename(&swap.fresh, &swap.target) {
                let _ = fs::rename(&swap.aside, &swap.target);
                return Err(e);
            }
            discard(&swap.aside, swap.new);
        }
    } else if target == Some(swap.new) && aside == Some(old) && fresh.is_none() {
        // Renamed aside, then the copy renamed in.
        rename(&swap.target, &swap.fresh)?;
        if let Err(e) = rename(&swap.aside, &swap.target) {
            let _ = fs::rename(&swap.fresh, &swap.target);
            return Err(e);
        }
    } else {
        return Err(lost());
    }
    if dir_at(&swap.target) != Some(old) {
        return Err(lost());
    }
    discard(&swap.fresh, swap.new);
    Ok("rolled_back")
}

/// Swap `swap`'s copy into place: through the held data directory when there is one
/// (still the recorded directory, on its parent's mount), by one rename when nothing is there.
fn swap_in(swap: &Swap, held: Option<&datadirs::Held>) -> Result<&'static str> {
    let failed = |why: String| StackError::new("snapshot_failed", format!("replacing {}'s data at {}: {why}", swap.service, swap.target.display()));
    match (swap.old, held) {
        (Some(old), Some(held)) => {
            if held.identity() != old {
                return Err(failed("it is not the directory that was checked".into()));
            }
            match held.on_parent_mount() {
                Ok(true) => {}
                Ok(false) => return Err(failed("it is a mount point; Stack never swaps a mount".into())),
                Err(e) => return Err(failed(format!("which mount it is on cannot be told ({e})"))),
            }
            let name = |p: &Path| p.file_name().unwrap_or_default().to_os_string();
            match held.swap_in(&name(&swap.fresh), &name(&swap.aside)).map_err(|e| failed(e.to_string()))? {
                datadirs::Swapped::Exchanged => Ok("exchange"),
                datadirs::Swapped::Renamed => Ok("rename"),
            }
        }
        (None, _) => {
            if fs::symlink_metadata(&swap.target).is_ok() {
                return Err(failed("something was put there since it was checked".into()));
            }
            fs::rename(&swap.fresh, &swap.target).map_err(|e| failed(e.to_string()))?;
            Ok("rename")
        }
        (Some(_), None) => Err(failed("it was not held open when checked".into())),
    }
}

#[cfg(test)]
thread_local! {
    /// Tests fail the swap of the service at this index, as an I/O error would.
    static FAIL_SWAP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Tests fail putting this index back, as an I/O error would.
    static FAIL_UNDO: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Tests stop the restore after this many swaps, as a crash would (the journal stays).
    static CRASH_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Tests fail recording a committed restore.
    static FAIL_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every service's swap undone: the outcome of each, and whether all are back as they were.
fn undo_all(swaps: &[Swap]) -> (Vec<Value>, bool) {
    let mut whole = true;
    let mut out = Vec::new();
    for (i, swap) in swaps.iter().enumerate().rev() {
        #[cfg(test)]
        let undone = if FAIL_UNDO.with(std::cell::Cell::get) == Some(i) { Err("putting it back failed (test)".to_string()) } else { undo(swap) };
        #[cfg(not(test))]
        let undone = undo(swap);
        let _ = i;
        out.push(match undone {
            Ok(outcome) => json!({ "service": swap.service, "outcome": outcome, "data_dir": swap.dir }),
            Err(why) => {
                whole = false;
                manual_recovery(swap, why)
            }
        });
    }
    out.reverse();
    (out, whole)
}

/// A service whose data Stack leaves where it is for a person to sort out: why, and where its
/// old data and the restored copy are, among the places the restore used.
fn manual_recovery(swap: &Swap, why: String) -> Value {
    let mut v = json!({ "service": swap.service, "outcome": "manual_recovery", "data_dir": swap.dir, "why": why });
    if let Some(old) = swap.old {
        if let Some(at) = [&swap.target, &swap.fresh, &swap.aside].into_iter().find(|p| dir_at(p) == Some(old)) {
            v["old_data"] = json!(at);
        }
    }
    if let Some(at) = [&swap.target, &swap.fresh, &swap.aside].into_iter().find(|p| dir_at(p) == Some(swap.new)) {
        v["restored_copy"] = json!(at);
    }
    v
}

/// The error for a restore that changed nothing in the end: every service's data is as it was.
fn unchanged(cause: StackError, services: Vec<Value>) -> StackError {
    let mut e = StackError::new("snapshot_failed", format!("{}; every service's data is as it was", cause.message)).with_detail(json!({ "services": services, "cause": cause.code }));
    e.details.extend(cause.details);
    e.hint = cause.hint;
    e
}

/// The error for a restore that left data in a state the restore did not finish: what each
/// service holds and where its old data is, with the journal the next operation finishes from.
fn incomplete(message: String, services: Vec<Value>, journal: &Path) -> StackError {
    StackError::new("restore_incomplete", message)
        .hint("services left this way are not started; run the snapshot command again or `stack up`, which first finishes or undoes the interrupted restore. Data marked `manual_recovery` must be moved back by hand (details name where it is); then remove the journal named in details")
        .with_detail(json!({ "services": services, "journal": journal }))
}

impl Swap {
    fn rebind<'a>(&'a self, project: &'a Path) -> datadirs::Rebind<'a> {
        datadirs::Rebind { project, service: &self.service, dir: &self.dir, location: &self.target, from: self.old, to: self.new }
    }
}

/// Before a committed restore is recorded: the checkout is still the one that restored (its
/// token), each restored copy is still at its place as the very directory the restore made, and
/// each record still names this checkout's directory there (see [`datadirs::Rebind::matches`]).
/// Otherwise why, with what each service holds.
fn check_committed(state: &Path, root: &Path, journal: &Journal) -> std::result::Result<(), (String, Vec<Value>)> {
    let token = crate::checkout::token(root);
    let entries = datadirs::entries(state);
    let mut first = None;
    let services = journal
        .swaps
        .iter()
        .map(|swap| {
            let why = if token.as_deref() != Some(journal.token.as_str()) {
                Some(format!("{} is no longer the checkout that restored it", root.display()))
            } else if dir_at(&swap.target) != Some(swap.new) {
                Some(format!("{} is no longer the restored copy", swap.target.display()))
            } else {
                match &entries {
                    Err(e) => Some(format!("Stack's record of data directories cannot be read ({})", e.message)),
                    Ok(entries) if !entries.iter().any(|e| swap.rebind(root).matches(e, &journal.token)) => {
                        Some(format!("Stack's record no longer names {} as {}'s data directory for this checkout", swap.dir.display(), swap.service))
                    }
                    Ok(_) => None,
                }
            };
            match why {
                None => json!({ "service": swap.service, "outcome": "restored", "data_dir": swap.dir }),
                Some(why) => {
                    first.get_or_insert_with(|| why.clone());
                    manual_recovery(swap, why)
                }
            }
        })
        .collect();
    first.map_or(Ok(()), |why| Err((why, services)))
}

#[cfg(test)]
thread_local! {
    /// Tests fail recording a committed restore once its init markers are written, as a crash
    /// between them would.
    static FAIL_REBIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Record a committed restore, once [`check_committed`] passes: the init markers in one write,
/// each record bound to the directory that replaced it, the old data removed (kept where a
/// mount is inside it), the journal removed. Each step can run again after an interruption.
/// The error is `restore_incomplete`, with the journal and the old data kept.
fn commit(state: &Path, root: &Path, journal: &Journal, how: &[&str]) -> Result<Vec<Value>> {
    let path = journal_path(state, root);
    if let Err((why, services)) = check_committed(state, root, journal) {
        return Err(incomplete(
            format!("snapshot '{}' was restored, but {why}; nothing was recorded and the old data is kept", journal.snapshot),
            services,
            &path,
        ));
    }
    let recorded = (|| -> Result<()> {
        #[cfg(test)]
        if FAIL_COMMIT.with(std::cell::Cell::get) {
            return Err(StackError::new("state_invalid", "recording failed (test)"));
        }
        init_tasks::set_markers(state, root, &journal.markers)?;
        #[cfg(test)]
        if FAIL_REBIND.with(std::cell::Cell::get) {
            return Err(StackError::new("state_invalid", "binding failed (test)"));
        }
        let rebinds: Vec<datadirs::Rebind> = journal.swaps.iter().map(|s| s.rebind(root)).collect();
        datadirs::rebind(state, &journal.token, &rebinds)
    })();
    recorded.map_err(|e| {
        incomplete(
            format!("snapshot '{}' was restored, but recording it failed: {}; the restored data stays, and the next snapshot command or `stack up` records it", journal.snapshot, e.message),
            restored(journal, how),
            &path,
        )
    })?;
    let mut kept = Vec::new();
    for swap in &journal.swaps {
        let Some(old) = swap.old else { continue };
        let Some(at) = [&swap.fresh, &swap.aside].into_iter().find(|p| dir_at(p) == Some(old)) else { continue };
        if let Err(e) = datadirs::remove_tree(at, old) {
            kept.push(json!({ "service": swap.service, "old_data_kept": at, "why": e.to_string() }));
        }
    }
    match fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(incomplete(format!("snapshot '{}' was restored and recorded, but its journal cannot be removed: {e}", journal.snapshot), restored(journal, how), &path))
        }
        _ => {}
    }
    Ok(kept)
}

/// The journal of an interrupted restore of this checkout, if there is one. One that cannot be
/// read is an error: which services it touched cannot be told.
fn read_journal(state: &Path, root: &Path) -> Result<Option<Journal>> {
    let path = journal_path(state, root);
    match fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path.display(), e)),
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
            StackError::new("state_invalid", format!("{} cannot be read ({e})", path.display())).hint("remove it once the checkout's data directories are as they should be")
        }),
    }
}

/// Finish a restore of this checkout that was interrupted: undone when it had not committed,
/// recorded when it had. `None` when there is none. The caller holds the project lock and has
/// confirmed `stopped` stopped, which must cover every service the journal names ([`journaled`]
/// names them); otherwise nothing is touched.
pub fn recover_restore(state: &Path, root: &Path, stopped: &[String]) -> Result<Option<Value>> {
    let path = journal_path(state, root);
    let Some(journal) = read_journal(state, root)? else { return Ok(None) };
    let unconfirmed: Vec<&str> = journal.swaps.iter().map(|s| s.service.as_str()).filter(|s| !stopped.iter().any(|n| n == s)).collect();
    if !unconfirmed.is_empty() {
        return Err(StackError::new(
            "stop_unconfirmed",
            format!("an interrupted restore of snapshot '{}' covers {}, not confirmed stopped; nothing was changed", journal.snapshot, unconfirmed.join(", ")),
        )
        .hint("run `stack down`, then `stack up`"));
    }
    if journal.phase == "committed" {
        let kept = commit(state, root, &journal, &[])?;
        return Ok(Some(json!({ "snapshot": journal.snapshot, "interrupted_restore": "completed", "kept": kept })));
    }
    let (services, whole) = undo_all(&journal.swaps);
    if !whole {
        return Err(incomplete(format!("an interrupted restore of snapshot '{}' cannot be undone", journal.snapshot), services, &path));
    }
    let _ = fs::remove_file(&path);
    Ok(Some(json!({ "snapshot": journal.snapshot, "interrupted_restore": "rolled_back", "services": services })))
}

/// The services an interrupted restore of this checkout left, for the caller to confirm stopped
/// before [`recover_restore`]: every one the journal names, whether or not the configuration
/// still defines it. `None` when there is none; an error when the journal cannot be read.
pub fn journaled(state: &Path, root: &Path) -> Result<Option<Vec<String>>> {
    Ok(read_journal(state, root)?.map(|j| j.swaps.into_iter().map(|s| s.service).collect()))
}

/// `restore_incomplete` for a command that would start services or claim their data while a
/// restore of this checkout is still to be finished or undone (`restart`): it does neither.
pub fn refuse_pending(state: &Path, root: &Path, command: &str) -> Result<()> {
    let Some(services) = journaled(state, root)? else { return Ok(()) };
    Err(incomplete(
        format!("a snapshot restore of this checkout ({}) was interrupted; `{command}` changed nothing", services.join(", ")),
        services.iter().map(|s| json!({ "service": s, "outcome": "pending" })).collect(),
        &journal_path(state, root),
    ))
}

fn restored(journal: &Journal, how: &[&str]) -> Vec<Value> {
    journal.swaps.iter().enumerate().map(|(i, s)| json!({ "service": s.service, "outcome": "restored", "data_dir": s.dir, "replace": how.get(i) })).collect()
}

/// What a snapshot's init state makes the init marker of restored data (see [`crate::init_tasks`]):
/// done for the init list it completed; failed after the tasks it completed, so `up` resumes
/// from the failed one; none at all (init runs from the start) when it had not begun or
/// completed nothing.
fn restored_marker(name: &str, snap: &SnapService, data_dir: DirIdentity) -> Option<Marker> {
    let base = |status: &str, digest: String| Marker { data_dir: Some(data_dir.clone()), status: status.into(), digest, completed: Vec::new(), failed_task: None, source: format!("snapshot:{name}"), at: now() };
    if let Some(digest) = snap.init_digest.clone() {
        return Some(base("done", digest));
    }
    let resume = snap.init_resume.as_ref().filter(|r| !r.completed.is_empty())?;
    Some(Marker { completed: resume.completed.clone(), failed_task: resume.failed_task.clone(), ..base("failed", resume.digest.clone()) })
}

/// Restore each planned service from the snapshot `name` at `snapshot`, all or none (see above).
fn restore_services(ctx: &Ctx, name: &str, snapshot: &Path, plan: &[(String, SnapService, DataDir)]) -> Result<Vec<Value>> {
    let Some((_, _, first)) = plan.first() else { return Ok(Vec::new()) };
    let token = first.token.clone();
    let path = journal_path(&ctx.state, &ctx.root);
    let untouched = || plan.iter().map(|(s, _, d)| json!({ "service": s, "outcome": "unchanged", "data_dir": d.dir })).collect::<Vec<_>>();
    // 1. Every copy, before anything is replaced.
    let mut swaps: Vec<Swap> = Vec::new();
    let mut copies = Vec::new();
    for (service, _, data) in plan {
        let parent = data.location.parent().expect("checked absolute with a parent");
        let label = data.location.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let fresh = unique(parent, &format!(".{label}.stack-restore"));
        let copied = fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e)).and_then(|()| copy_tree(&snapshot.join("data").join(service), &fresh));
        let new = copied.and_then(|how| dir_at(&fresh).map(|id| (how, id)).ok_or_else(|| StackError::new("snapshot_failed", format!("the copy at {} is gone", fresh.display()))));
        let (how, new) = match new {
            Ok(v) => v,
            Err(e) => {
                remove_tree(&fresh);
                swaps.iter().for_each(|s| discard(&s.fresh, s.new));
                return Err(unchanged(e, untouched()));
            }
        };
        let old = data.held.as_ref().map(datadirs::Held::identity);
        swaps.push(Swap { service: service.clone(), dir: data.dir.clone(), target: data.location.clone(), fresh, aside: unique(parent, &format!(".{label}.stack-old")), old, new });
        copies.push(how);
    }
    // 2. The journal.
    let markers = plan.iter().zip(&swaps).map(|((service, snap, data), swap)| (service.clone(), restored_marker(name, snap, DirIdentity { location: data.location.clone(), identity: swap.new }))).collect();
    let mut journal = Journal { snapshot: name.to_string(), phase: "swapping".into(), token, swaps, markers };
    if let Err(e) = write_json(&path, &journal) {
        journal.swaps.iter().for_each(|s| discard(&s.fresh, s.new));
        return Err(unchanged(e, untouched()));
    }
    // 3. The swaps.
    let mut how = Vec::new();
    for (i, (swap, (_, _, data))) in journal.swaps.iter().zip(plan).enumerate() {
        let _ = i;
        #[cfg(test)]
        if CRASH_AFTER.with(std::cell::Cell::get) == Some(i) {
            return Err(StackError::new("snapshot_failed", "crashed (test)"));
        }
        #[cfg(test)]
        let swapped = if FAIL_SWAP.with(std::cell::Cell::get) == Some(i) { Err(StackError::new("snapshot_failed", format!("replacing {}'s data failed (test)", swap.service))) } else { swap_in(swap, data.held.as_ref()) };
        #[cfg(not(test))]
        let swapped = swap_in(swap, data.held.as_ref());
        match swapped {
            Ok(h) => how.push(h),
            Err(e) => return Err(roll_back(&journal, &path, e, &swap.service)),
        }
    }
    // 4. Committed: the restore has happened.
    journal.phase = "committed".into();
    if let Err(e) = write_json(&path, &journal) {
        journal.phase = "swapping".into();
        return Err(roll_back(&journal, &path, e, "the journal"));
    }
    let kept = commit(&ctx.state, &ctx.root, &journal, &how)?;
    Ok(journal
        .swaps
        .iter()
        .zip(copies.iter().zip(&how))
        .map(|(s, (copy, replace))| {
            let mut v = json!({ "service": s.service, "data_dir": s.target, "copy": copy, "replace": replace });
            if let Some(k) = kept.iter().find(|k| k["service"] == json!(s.service)) {
                v["old_data_kept"] = k["old_data_kept"].clone();
            }
            v
        })
        .collect())
}

/// Undo every swap of a restore that failed at `failed`: `snapshot_failed` when every service
/// is back as it was (the journal removed), `restore_incomplete` otherwise (the journal kept).
fn roll_back(journal: &Journal, path: &Path, cause: StackError, failed: &str) -> StackError {
    let (services, whole) = undo_all(&journal.swaps);
    if whole {
        let _ = fs::remove_file(path);
        return unchanged(cause, services).with_detail(json!({ "failed": failed }));
    }
    incomplete(format!("{}; restoring snapshot '{}' failed and not every service could be put back as it was", cause.message, journal.snapshot), services, path)
        .with_detail(json!({ "failed": failed, "cause": cause.code }))
}

// ---- operations ---------------------------------------------------------------------------

fn compile_readonly(ctx: &Ctx) -> Result<Report> {
    project::compile_locked(&Options {
        root: ctx.root.clone(),
        mode: Mode::Frozen,
        write: false,
        cache: ctx.cache.clone(),
        state: ctx.state.clone(),
        reassign_ports: false,
        resolver: None,
        locker: None,
    })
}

#[derive(Debug, Serialize)]
pub struct SnapshotReport {
    pub name: String,
    pub project: ProjectId,
    pub services: Vec<String>,
    pub steps: Vec<Value>,
    pub restarted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<session::Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

/// The services a save covers: those named, else every supported one.
fn chosen(report: &Report, named: &[String]) -> Result<Vec<String>> {
    let services: Vec<String> = if named.is_empty() {
        report.stack.services.keys().filter(|s| preset(report, s).is_some_and(|p| SUPPORTED.contains(&p))).cloned().collect()
    } else {
        named.to_vec()
    };
    for service in &services {
        match preset(report, service) {
            Some(p) if SUPPORTED.contains(&p) => {}
            _ if !report.stack.services.contains_key(service) => {
                return Err(StackError::new("unknown_service", format!("no service named '{service}'")))
            }
            other => {
                return Err(StackError::new(
                    "snapshot_unsupported",
                    format!("service '{service}' is {}; snapshots cover postgres and redis presets only", other.map_or("a custom command".to_string(), |p| format!("a {p} preset"))),
                )
                .hint("name only postgres or redis services with --service"))
            }
        }
    }
    if services.is_empty() {
        return Err(StackError::new("snapshot_unsupported", "this project has no postgres or redis service to snapshot"));
    }
    Ok(services)
}

pub fn save(ctx: &Ctx, name: &str, named: &[String], force: bool) -> Result<SnapshotReport> {
    valid_name(name)?;
    let project = project_id(&ctx.root);
    let report = compile_readonly(ctx)?;
    let services = chosen(&report, named)?;
    let final_dir = store(&ctx.state, &project).join(name);
    if generation(&ctx.state, &project, name)?.is_some() && !force {
        return Err(StackError::new("snapshot_exists", format!("snapshot '{name}' already exists for this project"))
            .hint(format!("pick another name, `stack snapshot rm {name}`, or `stack snapshot save {name} --force` to replace it")));
    }
    let _run = init_tasks::run_lock(&ctx.state, &ctx.root)?;
    let stopped = session::with_stopped(ctx, &services, |report, statuses, confirmed| {
        // Never a copy of data a restore left half done.
        let interrupted = recover_restore(&ctx.state, &ctx.root, confirmed)?;
        let _store = store_lock(&ctx.state, &project)?;
        let base = store(&ctx.state, &project);
        fs::create_dir_all(&base).map_err(|e| io_error(base.display(), e))?;
        let recovered = recover_store(&base)?;
        let tmp = unique(&base, &format!(".tmp-{name}"));
        fs::create_dir(&tmp).map_err(|e| io_error(tmp.display(), e))?;
        fs::create_dir(tmp.join("data")).map_err(|e| io_error(tmp.display(), e))?;
        let built = (|| -> Result<(Meta, Vec<Value>)> {
            let markers = init_tasks::read_markers(&ctx.state, &ctx.root)?;
            let mut meta = Meta { name: name.to_string(), format: 1, project: project.id.clone(), created_at: now(), source: ctx.root.clone(), services: IndexMap::new() };
            let mut copied = Vec::new();
            for service in &services {
                let data = data_dir(ctx, statuses, service)?;
                if !data.exists {
                    return Err(StackError::new("snapshot_failed", format!("'{service}' has no data yet ({} does not exist)", data.dir.display()))
                        .hint("run `stack up` first"));
                }
                let how = copy_tree(&data.location, &tmp.join("data").join(service))?;
                let tally = check_tree(&tmp.join("data").join(service))?;
                let dir = &data.dir;
                let dir_str = dir.to_string_lossy().into_owned();
                let marker = markers.get(service);
                let init = init_tasks::classify(&report.stack, service, marker, Some(&dir_str));
                let init_digest = matches!(init, InitState::Done | InitState::Stale).then(|| marker.map(|m| m.digest.clone())).flatten();
                let init_resume = (init == InitState::Failed)
                    .then(|| marker.map(|m| Resume { digest: m.digest.clone(), completed: m.completed.clone(), failed_task: m.failed_task.clone() }))
                    .flatten();
                let version = locked_version(report, service).unwrap_or_default();
                meta.services.insert(service.clone(), SnapService {
                    preset: preset(report, service).unwrap_or_default().to_string(),
                    major: major(&version),
                    version,
                    files: tally.files,
                    bytes: tally.bytes,
                    init,
                    init_digest,
                    init_resume,
                });
                copied.push(json!({ "service": service, "from": dir, "copy": how, "files": tally.files, "bytes": tally.bytes }));
            }
            write_json(&tmp.join(META), &meta)?;
            Ok((meta, copied))
        })();
        let (meta, copied) = match built {
            Ok(b) => b,
            Err(e) => {
                remove_tree(&tmp);
                return Err(e);
            }
        };
        if fs::symlink_metadata(&final_dir).is_ok() && !force {
            remove_tree(&tmp);
            return Err(StackError::new("snapshot_exists", format!("snapshot '{name}' was created while this one was saved")));
        }
        let published = publish(&base, name, &tmp, &final_dir).inspect_err(|_| remove_tree(&tmp))?;
        let mut out = json!({ "copied": copied, "meta": meta, "publish": published });
        if !recovered.is_empty() {
            out["recovered"] = json!(recovered);
        }
        if let Some(interrupted) = interrupted {
            out["interrupted_restore"] = interrupted;
        }
        Ok(out)
    })?;
    let meta = serde_json::from_value(stopped.result["meta"].clone()).ok();
    Ok(SnapshotReport { name: name.to_string(), project, services, steps: stopped.steps, restarted: stopped.restarted, checks: stopped.checks, meta })
}

pub fn restore(ctx: &Ctx, name: &str) -> Result<SnapshotReport> {
    valid_name(name)?;
    let project = project_id(&ctx.root);
    let chosen = read_generation(&ctx.state, &project, name)?;
    compatible(&chosen.meta, &compile_readonly(ctx)?)?;
    let services: Vec<String> = chosen.meta.services.keys().cloned().collect();
    let _run = init_tasks::run_lock(&ctx.state, &ctx.root)?;
    let stopped = session::with_stopped(ctx, &services, |report, statuses, confirmed| {
        let interrupted = recover_restore(&ctx.state, &ctx.root, confirmed)?;
        let _store = store_lock(&ctx.state, &project)?;
        recover_store(&store(&ctx.state, &project))?;
        // Read again under the lock: it may have been replaced or removed meanwhile, by a
        // `save --force` of another checkout. Only the generation whose services were stopped
        // is restored; another is refused before anything is replaced.
        let now = read_generation(&ctx.state, &project, name)?;
        check_same(&chosen, &now, confirmed)?;
        compatible(&now.meta, report)?;
        // Every directory is checked, and held, before any is replaced.
        let plan: Vec<(String, SnapService, DataDir)> =
            now.meta.services.iter().map(|(service, snap)| Ok((service.clone(), snap.clone(), data_dir(ctx, statuses, service)?))).collect::<Result<_>>()?;
        let restored = restore_services(ctx, name, &now.dir, &plan)?;
        let mut out = json!({ "restored": restored });
        if let Some(interrupted) = interrupted {
            out["interrupted_restore"] = interrupted;
        }
        Ok(out)
    })?;
    Ok(SnapshotReport { name: name.to_string(), project, services, steps: stopped.steps, restarted: stopped.restarted, checks: stopped.checks, meta: Some(chosen.meta) })
}

/// The generation read under the store lock (`now`) is the one `chosen` before its services
/// were stopped, and those services are all confirmed stopped: `snapshot_failed` otherwise, with
/// nothing replaced.
fn check_same(chosen: &Generation, now: &Generation, confirmed: &[String]) -> Result<()> {
    let name = &chosen.meta.name;
    if !chosen.same(now) {
        return Err(StackError::new("snapshot_failed", format!("snapshot '{name}' was replaced while its services were stopped for the restore; nothing was restored"))
            .hint(format!("retry `stack snapshot restore {name}` to restore the snapshot as it is now")));
    }
    if let Some(service) = now.meta.services.keys().find(|s| !confirmed.contains(s)) {
        return Err(StackError::new("stop_unconfirmed", format!("'{service}' was not confirmed stopped; nothing was restored")).hint("run `stack down`, then retry"));
    }
    Ok(())
}

/// `up --from-snapshot`: refused before anything starts when the snapshot is missing or does
/// not fit this checkout's services.
pub fn check_up(ctx: &Ctx, report: &Report, options: &UpOptions) -> Result<()> {
    let Some(name) = options.from_snapshot.as_deref() else { return Ok(()) };
    valid_name(name)?;
    if report.stack.services.is_empty() {
        return Err(StackError::new("snapshot_unsupported", "this project defines no services to restore"));
    }
    let project = project_id(&ctx.root);
    compatible(&read_meta(&ctx.state, &project, name)?, report)
}

/// `up --from-snapshot`, under `up`'s project lock and before the first start: restore each
/// snapshotted service whose data directory does not exist yet. A service that already has
/// data keeps it (reported as skipped), so the same `up` can run on every checkout creation.
pub fn restore_before_start(ctx: &Ctx, report: &Report, statuses: &[DaemonStatus], name: &str) -> Result<Value> {
    let project = project_id(&ctx.root);
    let _store = store_lock(&ctx.state, &project)?;
    recover_store(&store(&ctx.state, &project))?;
    let Generation { dir: snapshot, meta, .. } = read_generation(&ctx.state, &project, name)?;
    compatible(&meta, report)?;
    let unresolved = crate::supervisor::unresolved(&crate::supervisor::dir(&ctx.state, &ctx.root));
    if !unresolved.is_empty() {
        return Err(StackError::new("stop_unconfirmed", "processes of this checkout's services may still run; nothing was restored")
            .hint("run `stack down`, which stops what it can show is this checkout's and names the rest, then retry")
            .details(unresolved));
    }
    let mut skipped = Vec::new();
    let mut plan = Vec::new();
    for (service, snap) in &meta.services {
        let data = data_dir(ctx, statuses, service)?;
        if data.exists {
            skipped.push(json!({ "service": service, "data_dir": data.dir, "reason": "data directory exists" }));
            continue;
        }
        // Nothing can be running on a directory that does not exist; still, never copy under
        // a running daemon.
        if statuses.iter().any(|d| d.name == *service && session::may_run(d)) {
            return Err(StackError::new("snapshot_failed", format!("'{service}' is running; nothing was restored")).hint("run `stack down`, then retry"));
        }
        plan.push((service.clone(), snap.clone(), data));
    }
    let restored = restore_services(ctx, name, &snapshot, &plan)?;
    Ok(json!({ "snapshot": name, "restored": restored, "skipped": skipped }))
}

/// `up`, under its project lock and before anything starts or claims data: finish a restore of
/// this checkout that was interrupted (see [`recover_restore`]), provided none of its services
/// may still run. `None` when there was none.
pub fn recover_before_start(ctx: &Ctx, statuses: &[DaemonStatus]) -> Result<Option<Value>> {
    let Some(services) = journaled(&ctx.state, &ctx.root)? else { return Ok(None) };
    let running: Vec<&String> = services.iter().filter(|s| statuses.iter().any(|d| d.name == **s && session::may_run(d))).collect();
    let unresolved = crate::supervisor::unresolved(&crate::supervisor::dir(&ctx.state, &ctx.root));
    if !running.is_empty() || !unresolved.is_empty() {
        return Err(StackError::new("stop_unconfirmed", "a snapshot restore of this checkout was interrupted, and its services may still run; nothing was changed")
            .hint("run `stack down`, then `stack up`")
            .with_detail(json!({ "running": running }))
            .details(unresolved));
    }
    recover_restore(&ctx.state, &ctx.root, &services)
}

#[derive(Debug, Serialize)]
pub struct ListReport {
    pub project: ProjectId,
    pub snapshots: Vec<Meta>,
}

pub fn list(ctx: &Ctx) -> Result<ListReport> {
    let project = project_id(&ctx.root);
    let mut snapshots = Vec::new();
    for name in names(&ctx.state, &project) {
        if let Some((_, meta, _)) = lookup(&store(&ctx.state, &project), &name)? {
            snapshots.push(meta);
        }
    }
    Ok(ListReport { project, snapshots })
}

pub fn rm(ctx: &Ctx, name: &str) -> Result<Value> {
    valid_name(name)?;
    let project = project_id(&ctx.root);
    let _store = store_lock(&ctx.state, &project)?;
    recover_store(&store(&ctx.state, &project))?;
    let dir = store(&ctx.state, &project).join(name);
    if !dir.join(META).is_file() {
        return Err(not_found(&ctx.state, &project, name));
    }
    // Out of the listing in one step, then deleted.
    let gone = unique(&store(&ctx.state, &project), &format!(".rm-{name}"));
    fs::rename(&dir, &gone).map_err(|e| io_error(dir.display(), e))?;
    remove_tree(&gone);
    Ok(json!({ "removed": name, "project": project }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn copies_drop_run_files_keep_modes_and_carry_no_server_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a/data/postgres");
        write(&src.join("PG_VERSION"), "17\n");
        write(&src.join("base/1/1259"), "rows");
        write(&src.join("postmaster.opts"), &format!("postgres \"-D\" \"{}\"", src.display()));
        write(&src.join("postmaster.pid"), "123\n");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o700)).unwrap();
        let dst = dir.path().join("b/data/postgres");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        copy_tree(&src, &dst).unwrap();
        assert_eq!(fs::read_to_string(dst.join("base/1/1259")).unwrap(), "rows");
        assert!(!dst.join("postmaster.pid").exists() && !dst.join("postmaster.opts").exists());
        assert_eq!(fs::metadata(&dst).unwrap().permissions().mode() & 0o777, 0o700);
        // The plain copy too.
        let plain = dir.path().join("c");
        copy_dir(&src, &plain, true).unwrap();
        assert!(!plain.join("postmaster.opts").exists());
        assert_eq!(fs::metadata(&plain).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn links_out_of_the_data_directory_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("data");
        write(&src.join("PG_VERSION"), "17\n");
        fs::create_dir_all(src.join("pg_tblspc")).unwrap();
        std::os::unix::fs::symlink(dir.path(), src.join("pg_tblspc/16384")).unwrap();
        let e = copy_tree(&src, &dir.path().join("copy")).unwrap_err();
        assert_eq!(e.code, "snapshot_unsupported", "{e}");
        assert!(!dir.path().join("copy").exists());
    }

    /// A checkout whose services `names` each have recorded data (`old-<service>`, or nothing
    /// when `existing` is false), and a snapshot holding `new-<service>` for each.
    struct Restore {
        _dir: tempfile::TempDir,
        ctx: Ctx,
        snapshot: PathBuf,
        names: Vec<String>,
    }

    impl Restore {
        fn new(names: &[&str], existing: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().canonicalize().unwrap();
            let ctx = Ctx { root: base.join("checkout"), cache: base.join("cache"), state: base.join("state") };
            fs::create_dir_all(&ctx.root).unwrap();
            let snapshot = base.join("snap");
            for name in names {
                write(&snapshot.join("data").join(name).join("file"), &format!("new-{name}"));
                if existing {
                    write(&base.join("data").join(name).join("file"), &format!("old-{name}"));
                }
            }
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            let r = Restore { _dir: dir, ctx, snapshot, names };
            let token = r.token();
            for name in &r.names {
                let target = r.target(name);
                let (location, identity) = datadirs::locate(&target);
                let entry = datadirs::Entry { project: r.ctx.root.clone(), service: name.clone(), dir: target, checkout: token.clone(), supervisor: None, location, identity, project_at: Default::default() };
                datadirs::record(&r.ctx.state, entry).unwrap();
            }
            r
        }

        fn token(&self) -> String {
            crate::checkout::token(&self.ctx.root).unwrap()
        }

        fn target(&self, name: &str) -> PathBuf {
            self.ctx.state.parent().unwrap().join("data").join(name)
        }

        fn plan(&self, snap: &SnapService) -> Vec<(String, SnapService, DataDir)> {
            self.names
                .iter()
                .map(|name| {
                    let target = self.target(name);
                    let entry = datadirs::entries(&self.ctx.state).unwrap().into_iter().find(|e| e.service == *name).unwrap();
                    let held = match datadirs::hold(&entry) {
                        Found::Held(h) => Some(h),
                        _ => None,
                    };
                    (name.clone(), snap.clone(), DataDir { dir: target.clone(), location: target, exists: held.is_some(), token: self.token(), held })
                })
                .collect()
        }

        fn run(&self) -> Result<Vec<Value>> {
            let snap = SnapService { preset: "postgres".into(), version: "17.1".into(), major: "17".into(), files: 1, bytes: 1, init: InitState::Done, init_digest: Some("d1".into()), init_resume: None };
            restore_services(&self.ctx, "base", &self.snapshot, &self.plan(&snap))
        }

        fn contents(&self) -> Vec<String> {
            self.names.iter().map(|n| fs::read_to_string(self.target(n).join("file")).unwrap_or_else(|_| "-".into())).collect()
        }

        fn identities(&self) -> Vec<Option<[u64; 2]>> {
            self.names.iter().map(|n| dir_at(&self.target(n))).collect()
        }

        /// What is in the data directories' parent besides the data directories.
        fn leftovers(&self) -> Vec<String> {
            let parent = self.target("x").parent().unwrap().to_path_buf();
            fs::read_dir(parent).map(|d| d.flatten().map(|e| e.file_name().into_string().unwrap()).filter(|n| !self.names.contains(n)).collect()).unwrap_or_default()
        }

        fn journal(&self) -> Option<Journal> {
            serde_json::from_str(&fs::read_to_string(journal_path(&self.ctx.state, &self.ctx.root)).ok()?).ok()
        }

        fn markers(&self) -> IndexMap<String, Marker> {
            init_tasks::read_markers(&self.ctx.state, &self.ctx.root).unwrap()
        }

        fn outcomes(e: &StackError) -> Vec<(String, String)> {
            let services = e.details.iter().find_map(|d| d["services"].as_array()).expect("per-service outcomes");
            services.iter().map(|s| (s["service"].as_str().unwrap().to_string(), s["outcome"].as_str().unwrap().to_string())).collect()
        }
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// Run `f` with the two-rename path of filesystems without an exchange, and with it.
    fn both_ways(f: impl Fn(bool)) {
        for no_exchange in [false, true] {
            datadirs::NO_EXCHANGE.with(|c| c.set(no_exchange));
            f(no_exchange);
        }
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
    }

    fn inject(swap: Option<usize>, undo: Option<usize>, crash: Option<usize>, commit: bool) {
        FAIL_SWAP.with(|c| c.set(swap));
        FAIL_UNDO.with(|c| c.set(undo));
        CRASH_AFTER.with(|c| c.set(crash));
        FAIL_COMMIT.with(|c| c.set(commit));
    }

    #[test]
    fn a_restore_replaces_every_service_and_records_them_at_once() {
        both_ways(|no_exchange| {
            let r = Restore::new(&["db", "cache", "queue"], true);
            let before = r.identities();
            let restored = r.run().unwrap();
            assert_eq!(r.contents(), ["new-db", "new-cache", "new-queue"]);
            assert!(restored.iter().all(|v| v["replace"] == if no_exchange { "rename" } else { "exchange" }), "{restored:?}");
            assert!(r.leftovers().is_empty(), "old data and copies are gone: {:?}", r.leftovers());
            assert!(r.journal().is_none());
            let markers = r.markers();
            for (name, was) in r.names.iter().zip(before) {
                let m = &markers[name];
                assert_eq!((m.status.as_str(), m.digest.as_str(), m.source.as_str()), ("done", "d1", "snapshot:base"));
                assert_eq!(m.data_dir.as_ref().unwrap().identity, dir_at(&r.target(name)).unwrap(), "the marker names the restored directory");
                assert_ne!(Some(m.data_dir.as_ref().unwrap().identity), was);
                let entry = datadirs::entries(&r.ctx.state).unwrap().into_iter().find(|e| e.service == *name).unwrap();
                assert_eq!(entry.identity, dir_at(&r.target(name)), "the record is bound to the restored directory");
            }
        });
        // Into data directories that do not exist yet (`up --from-snapshot`).
        let r = Restore::new(&["db", "cache"], false);
        r.run().unwrap();
        assert_eq!(r.contents(), ["new-db", "new-cache"]);
        assert!(r.leftovers().is_empty());
    }

    #[test]
    fn a_swap_failing_midway_puts_back_every_service_it_replaced() {
        both_ways(|_| {
            let r = Restore::new(&["db", "cache", "queue"], true);
            init_tasks::set_marker(&r.ctx.state, &r.ctx.root, "db", Some(Marker { data_dir: None, status: "done".into(), digest: "mine".into(), completed: Vec::new(), failed_task: None, source: "up".into(), at: 1 })).unwrap();
            let before = r.identities();
            inject(Some(2), None, None, false);
            let e = r.run().unwrap_err();
            inject(None, None, None, false);
            assert_eq!(e.code, "snapshot_failed", "{e}");
            assert!(e.message.contains("every service's data is as it was"), "{e}");
            assert_eq!(Restore::outcomes(&e), pairs(&[("db", "rolled_back"), ("cache", "rolled_back"), ("queue", "unchanged")]));
            assert!(e.details.iter().any(|d| d["failed"] == "queue"), "{e:?}");
            assert_eq!(r.contents(), ["old-db", "old-cache", "old-queue"]);
            assert_eq!(r.identities(), before, "the very directories that were there are back");
            assert!(r.leftovers().is_empty(), "{:?}", r.leftovers());
            assert!(r.journal().is_none());
            assert_eq!(r.markers()["db"].digest, "mine", "nothing was recorded as restored");
            assert_eq!(r.markers().len(), 1);
        });
        // A copy that fails: nothing was replaced at all.
        let r = Restore::new(&["db", "cache"], true);
        fs::remove_dir_all(r.snapshot.join("data/cache")).unwrap();
        let e = r.run().unwrap_err();
        assert_eq!(Restore::outcomes(&e), pairs(&[("db", "unchanged"), ("cache", "unchanged")]));
        assert_eq!(r.contents(), ["old-db", "old-cache"]);
        assert!(r.leftovers().is_empty() && r.journal().is_none());
    }

    #[test]
    fn a_data_directory_replaced_after_its_check_is_never_swapped() {
        let r = Restore::new(&["db", "cache"], true);
        let snap = SnapService { preset: "postgres".into(), version: "17.1".into(), major: "17".into(), files: 1, bytes: 1, init: InitState::None, init_digest: None, init_resume: None };
        let plan = r.plan(&snap);
        // Something else put at cache's place once it was checked and held.
        fs::rename(r.target("cache"), r.ctx.state.parent().unwrap().join("moved")).unwrap();
        write(&r.target("cache").join("file"), "intruder");
        let e = restore_services(&r.ctx, "base", &r.snapshot, &plan).unwrap_err();
        assert_eq!(e.code, "snapshot_failed", "{e}");
        assert!(e.message.contains("replaced since it was checked"), "{e}");
        assert_eq!(Restore::outcomes(&e), pairs(&[("db", "rolled_back"), ("cache", "unchanged")]));
        assert_eq!(r.contents(), ["old-db", "intruder"], "what replaced it is left alone");
    }

    #[test]
    fn a_service_that_cannot_be_put_back_keeps_its_old_data_and_is_reported() {
        both_ways(|no_exchange| {
            let r = Restore::new(&["db", "cache"], true);
            let before = r.identities();
            inject(Some(1), Some(0), None, false);
            let e = r.run().unwrap_err();
            inject(None, None, None, false);
            assert_eq!(e.code, "restore_incomplete", "{e}");
            assert_eq!(Restore::outcomes(&e), pairs(&[("db", "manual_recovery"), ("cache", "unchanged")]));
            let db = e.details.iter().find_map(|d| d["services"].as_array()).unwrap()[0].clone();
            let old_data = PathBuf::from(db["old_data"].as_str().unwrap());
            assert_eq!(fs::read_to_string(old_data.join("file")).unwrap(), "old-db", "{db}");
            assert_eq!(dir_at(&old_data), before[0]);
            assert_eq!(db["restored_copy"], json!(r.target("db")), "{db}");
            assert!(old_data.file_name().unwrap().to_str().unwrap().contains(if no_exchange { ".stack-old-" } else { ".stack-restore-" }));
            assert!(e.details.iter().any(|d| d["journal"].is_string()), "{e:?}");
            assert_eq!(r.journal().unwrap().phase, "swapping", "the journal stays for the next operation");
            assert!(r.markers().is_empty(), "nothing was recorded as restored");
            // The next operation undoes it.
            let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
            assert_eq!(recovered["interrupted_restore"], "rolled_back", "{recovered}");
            assert_eq!(r.contents(), ["old-db", "old-cache"]);
            assert_eq!(r.identities(), before);
            assert!(r.leftovers().is_empty() && r.journal().is_none(), "{:?}", r.leftovers());
            assert_eq!(recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap(), None);
        });
    }

    #[test]
    fn an_interrupted_restore_is_undone_before_it_commits_and_finished_after() {
        both_ways(|_| {
            // Crashed with two of three services swapped.
            let r = Restore::new(&["db", "cache", "queue"], true);
            let before = r.identities();
            inject(None, None, Some(2), false);
            r.run().unwrap_err();
            inject(None, None, None, false);
            assert_eq!(r.contents(), ["new-db", "new-cache", "old-queue"]);
            assert_eq!(journaled(&r.ctx.state, &r.ctx.root).unwrap().unwrap(), ["db", "cache", "queue"]);
            let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
            assert_eq!(recovered["interrupted_restore"], "rolled_back");
            let outcomes: Vec<&str> = recovered["services"].as_array().unwrap().iter().map(|s| s["outcome"].as_str().unwrap()).collect();
            assert_eq!(outcomes, ["rolled_back", "rolled_back", "unchanged"]);
            assert_eq!(r.contents(), ["old-db", "old-cache", "old-queue"]);
            assert_eq!(r.identities(), before);
            assert!(r.leftovers().is_empty() && r.journal().is_none() && r.markers().is_empty());

            // Every swap done, recording it failed: the restored data stays, and is recorded next.
            inject(None, None, None, true);
            let e = r.run().unwrap_err();
            inject(None, None, None, false);
            assert_eq!(e.code, "restore_incomplete", "{e}");
            assert_eq!(Restore::outcomes(&e), pairs(&[("db", "restored"), ("cache", "restored"), ("queue", "restored")]));
            assert_eq!(r.journal().unwrap().phase, "committed");
            assert!(r.markers().is_empty(), "not recorded yet");
            let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
            assert_eq!(recovered["interrupted_restore"], "completed", "{recovered}");
            assert_eq!(r.contents(), ["new-db", "new-cache", "new-queue"]);
            assert_eq!(r.markers().len(), 3);
            assert!(r.leftovers().is_empty() && r.journal().is_none(), "{:?}", r.leftovers());
        });
    }

    /// A restore of `names` that swapped every service and committed, but was not recorded.
    fn committed_unrecorded(names: &[&str]) -> (Restore, Vec<Option<[u64; 2]>>) {
        let r = Restore::new(names, true);
        let before = r.identities();
        inject(None, None, None, true);
        assert_eq!(r.run().unwrap_err().code, "restore_incomplete");
        inject(None, None, None, false);
        assert_eq!(r.journal().unwrap().phase, "committed");
        (r, before)
    }

    /// Where the old data of each journaled service is now (fresh or aside), if anywhere.
    fn old_data(r: &Restore) -> Vec<Option<PathBuf>> {
        r.journal().unwrap().swaps.iter().map(|s| [&s.fresh, &s.aside].into_iter().find(|p| dir_at(p) == s.old).cloned()).collect()
    }

    #[test]
    fn a_committed_restore_is_recorded_only_while_its_copies_checkout_and_records_are_as_it_left_them() {
        let ledger = |r: &Restore| datadirs::entries(&r.ctx.state).unwrap();
        // The restored copy at cache's place vanished, or another directory took its place.
        for replaced in [false, true] {
            let (r, before) = committed_unrecorded(&["db", "cache"]);
            let old = old_data(&r);
            let recorded = ledger(&r);
            fs::rename(r.target("cache"), r.ctx.state.parent().unwrap().join("elsewhere")).unwrap();
            if replaced {
                write(&r.target("cache").join("file"), "intruder");
            }
            let e = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap_err();
            assert_eq!(e.code, "restore_incomplete", "{e}");
            assert!(e.message.contains("is no longer the restored copy"), "{e}");
            assert_eq!(Restore::outcomes(&e), pairs(&[("db", "restored"), ("cache", "manual_recovery")]));
            let cache = e.details.iter().find_map(|d| d["services"].as_array()).unwrap()[1].clone();
            assert_eq!(dir_at(Path::new(cache["old_data"].as_str().unwrap())), before[1], "{cache}");
            assert!(r.markers().is_empty(), "no init marker is written");
            assert_eq!(ledger(&r), recorded, "no record is rebound");
            assert_eq!(old_data(&r), old, "no old data is removed");
            assert_eq!(r.journal().unwrap().phase, "committed", "the journal stays");
            // Again, the same answer: nothing moved meanwhile.
            assert_eq!(recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap_err().code, "restore_incomplete");
            assert_eq!(old_data(&r), old);
        }
        // Another checkout's token, and a record that no longer names this checkout's directory.
        for case in ["token", "ledger"] {
            let (r, _) = committed_unrecorded(&["db", "cache"]);
            let old = old_data(&r);
            match case {
                "token" => {
                    let mut journal = r.journal().unwrap();
                    journal.token = "another checkout".into();
                    write_json(&journal_path(&r.ctx.state, &r.ctx.root), &journal).unwrap();
                }
                _ => datadirs::forget(&r.ctx.state, |e| e.service == "db").unwrap(),
            }
            let recorded = ledger(&r);
            let e = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap_err();
            assert_eq!(e.code, "restore_incomplete", "{case}: {e}");
            let outcomes = Restore::outcomes(&e);
            assert_eq!(outcomes[0], ("db".to_string(), "manual_recovery".to_string()), "{case}: {e}");
            assert!(r.markers().is_empty() && ledger(&r) == recorded && old_data(&r) == old, "{case}: nothing recorded or removed");
            assert!(r.journal().is_some(), "{case}");
        }
    }

    #[test]
    fn recording_a_committed_restore_can_be_interrupted_anywhere_and_finished() {
        both_ways(|_| {
            let r = Restore::new(&["db", "cache"], true);
            // The init markers written, the records not bound yet.
            FAIL_REBIND.with(|c| c.set(true));
            let e = r.run().unwrap_err();
            FAIL_REBIND.with(|c| c.set(false));
            assert_eq!(e.code, "restore_incomplete", "{e}");
            assert_eq!(r.markers().len(), 2);
            let entry = |name: &str| datadirs::entries(&r.ctx.state).unwrap().into_iter().find(|e| e.service == name).unwrap();
            assert_ne!(entry("db").identity, dir_at(&r.target("db")), "not bound yet");
            let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
            assert_eq!(recovered["interrupted_restore"], "completed", "{recovered}");
            for name in ["db", "cache"] {
                assert_eq!(entry(name).identity, dir_at(&r.target(name)), "bound to the restored copy");
            }
            assert!(r.leftovers().is_empty() && r.journal().is_none(), "{:?}", r.leftovers());
            assert_eq!(r.contents(), ["new-db", "new-cache"]);
        });
        // Everything recorded, the journal still there (a crash before removing it): finished
        // again without complaint, the records already bound.
        let r = Restore::new(&["db"], true);
        r.run().unwrap();
        let mut journal = Journal { snapshot: "base".into(), phase: "committed".into(), token: r.token(), swaps: Vec::new(), markers: r.markers().into_iter().map(|(k, v)| (k, Some(v))).collect() };
        let target = r.target("db");
        journal.swaps.push(Swap { service: "db".into(), dir: target.clone(), target: target.clone(), fresh: target.with_file_name(".db.stack-restore-1-1"), aside: target.with_file_name(".db.stack-old-1-1"), old: Some([1, 1]), new: dir_at(&target).unwrap() });
        write_json(&journal_path(&r.ctx.state, &r.ctx.root), &journal).unwrap();
        let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
        assert_eq!(recovered["interrupted_restore"], "completed", "{recovered}");
        assert!(r.journal().is_none());
    }

    #[test]
    fn an_interrupted_restore_is_finished_only_with_every_service_it_names_confirmed_stopped() {
        let r = Restore::new(&["db", "cache"], true);
        let before = r.identities();
        inject(None, None, Some(1), false);
        r.run().unwrap_err();
        inject(None, None, None, false);
        assert_eq!(r.contents(), ["new-db", "old-cache"]);
        // Confirming only the service a command named is not enough.
        let e = recover_restore(&r.ctx.state, &r.ctx.root, &["cache".to_string()]).unwrap_err();
        assert_eq!(e.code, "stop_unconfirmed", "{e}");
        assert!(e.message.contains("db"), "{e}");
        assert_eq!(r.contents(), ["new-db", "old-cache"], "nothing was changed");
        assert!(r.journal().is_some());
        assert_eq!(refuse_pending(&r.ctx.state, &r.ctx.root, "restart").unwrap_err().code, "restore_incomplete");
        recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
        assert_eq!(r.identities(), before);
        refuse_pending(&r.ctx.state, &r.ctx.root, "restart").unwrap();
        // A journal that cannot be read names no services, and is never taken for none.
        write(&journal_path(&r.ctx.state, &r.ctx.root), "{ damaged");
        assert_eq!(journaled(&r.ctx.state, &r.ctx.root).unwrap_err().code, "state_invalid");
        assert_eq!(refuse_pending(&r.ctx.state, &r.ctx.root, "restart").unwrap_err().code, "state_invalid");
    }

    #[test]
    fn a_restore_goes_ahead_only_with_the_generation_whose_services_were_stopped() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        let confirmed = ["db".to_string()];
        let with_db = |path: &Path, created_at: u64, services: &[&str]| {
            snapshot_dir(path, "base", created_at, "data");
            let mut meta: Meta = serde_json::from_str(&fs::read_to_string(path.join(META)).unwrap()).unwrap();
            let snap = SnapService { preset: "postgres".into(), version: "17.1".into(), major: "17".into(), files: 1, bytes: 1, init: InitState::None, init_digest: None, init_resume: None };
            meta.services = services.iter().map(|s| (s.to_string(), snap.clone())).collect();
            write(&path.join(META), &serde_json::to_string(&meta).unwrap());
        };
        with_db(&base.join("base"), 1, &["db"]);
        let chosen = read_generation(state.path(), &project, "base").unwrap();
        check_same(&chosen, &read_generation(state.path(), &project, "base").unwrap(), &confirmed).unwrap();

        // Replaced by a forced save of another checkout, even within the same second, or with
        // another service set: refused before anything is replaced.
        for (created_at, services) in [(2, &["db"][..]), (1, &["db"][..]), (1, &["db", "cache"][..])] {
            let tmp = unique(&base, ".tmp-base");
            with_db(&tmp, created_at, services);
            publish(&base, "base", &tmp, &base.join("base")).unwrap();
            let now = read_generation(state.path(), &project, "base").unwrap();
            let e = check_same(&chosen, &now, &confirmed).unwrap_err();
            assert_eq!(e.code, "snapshot_failed", "{created_at} {services:?}: {e}");
            assert!(e.message.contains("was replaced"), "{e}");
        }

        // The chosen directory is removed by those saves, and a filesystem gives a freed inode to
        // the next directory made (Linux does at once). Saves of the same second and services,
        // each removing the one before, by exchange and by two renames, never publish a
        // generation on the chosen one's inode while the restore holds it, so none passes for it.
        assert_ne!(dir_at(&base.join("base")), Some(chosen.identity));
        for round in 0..64 {
            datadirs::NO_EXCHANGE.with(|c| c.set(round % 2 == 1));
            let tmp = unique(&base, ".tmp-base");
            with_db(&tmp, 1, &["db"]);
            publish(&base, "base", &tmp, &base.join("base")).unwrap();
            let now = read_generation(state.path(), &project, "base").unwrap();
            assert_ne!(now.identity, chosen.identity, "round {round}: a new generation on the chosen directory's inode");
            assert_eq!(check_same(&chosen, &now, &confirmed).unwrap_err().code, "snapshot_failed", "round {round}");
        }
        datadirs::NO_EXCHANGE.with(|c| c.set(false));

        // The same generation, put back from where an interrupted save set it aside, goes ahead.
        let _ = fs::remove_dir_all(&base);
        with_db(&base.join("base"), 1, &["db"]);
        let tmp = unique(&base, ".tmp-base");
        with_db(&tmp, 2, &["db"]);
        datadirs::NO_EXCHANGE.with(|c| c.set(true));
        FAIL_RENAMES.with(|c| c.set(&["publish", "put_back"]));
        publish(&base, "base", &tmp, &base.join("base")).unwrap_err();
        FAIL_RENAMES.with(|c| c.set(&[]));
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
        let chosen = read_generation(state.path(), &project, "base").unwrap();
        assert!(chosen.dir.file_name().unwrap().to_str().unwrap().starts_with(".old-base-"));
        recover_store(&base).unwrap();
        let now = read_generation(state.path(), &project, "base").unwrap();
        assert_eq!(now.dir, base.join("base"));
        check_same(&chosen, &now, &confirmed).unwrap();
        // A service of it that was not confirmed stopped is refused all the same.
        assert_eq!(check_same(&chosen, &now, &[]).unwrap_err().code, "stop_unconfirmed");
    }

    /// SnapshotAtomicity's RestoreSwap/RestoreSwap2 interleaved with CommandKilled: a restore
    /// killed between the two renames of a swap where the filesystem has no exchange (its data
    /// directory set aside, the copy not yet in place) is undone by the next recovery from
    /// where each directory is, with nothing lost and nothing recorded as restored.
    #[test]
    fn a_restore_killed_between_the_two_renames_of_a_swap_is_undone_from_where_each_directory_is() {
        datadirs::NO_EXCHANGE.with(|c| c.set(true));
        let r = Restore::new(&["db", "cache"], true);
        let before = r.identities();
        inject(None, None, Some(1), false);
        assert!(r.run().is_err());
        inject(None, None, None, false);
        // The first rename of the cache's swap, as `Held::swap_in` makes it, then the kill.
        let journal = r.journal().unwrap();
        assert_eq!(journal.phase, "swapping");
        let cache = journal.swaps.iter().find(|s| s.service == "cache").unwrap();
        fs::rename(&cache.target, &cache.aside).unwrap();
        assert_eq!(r.contents(), ["new-db", "-"]);
        assert_eq!(refuse_pending(&r.ctx.state, &r.ctx.root, "restart").unwrap_err().code, "restore_incomplete");
        let recovered = recover_restore(&r.ctx.state, &r.ctx.root, &r.names).unwrap().unwrap();
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
        assert_eq!(recovered["interrupted_restore"], "rolled_back", "{recovered}");
        let outcomes: Vec<&str> = recovered["services"].as_array().unwrap().iter().map(|s| s["outcome"].as_str().unwrap()).collect();
        assert_eq!(outcomes, ["rolled_back", "rolled_back"]);
        assert_eq!(r.contents(), ["old-db", "old-cache"]);
        assert_eq!(r.identities(), before);
        assert!(r.leftovers().is_empty() && r.journal().is_none() && r.markers().is_empty(), "{:?}", r.leftovers());
    }

    #[test]
    fn undoing_a_swap_reads_where_each_directory_is() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().canonicalize().unwrap();
        let (target, fresh, aside) = (p.join("data"), p.join(".data.stack-restore-1-1"), p.join(".data.stack-old-1-1"));
        let setup = |old_at: Option<&Path>, new_at: Option<&Path>| {
            for path in [&target, &fresh, &aside] {
                let _ = fs::remove_dir_all(path);
            }
            let mut ids = (None, [0, 0]);
            if let Some(at) = old_at {
                write(&at.join("file"), "old");
                ids.0 = dir_at(at);
            }
            if let Some(at) = new_at {
                write(&at.join("file"), "new");
                ids.1 = dir_at(at).unwrap();
            }
            Swap { service: "db".into(), dir: target.clone(), target: target.clone(), fresh: fresh.clone(), aside: aside.clone(), old: ids.0, new: ids.1 }
        };
        let content = |at: &Path| fs::read_to_string(at.join("file")).ok();
        // Not swapped; set aside only; exchanged; renamed aside and in.
        for (old_at, new_at, outcome) in [(&target, &fresh, "unchanged"), (&aside, &fresh, "rolled_back"), (&fresh, &target, "rolled_back"), (&aside, &target, "rolled_back")] {
            let swap = setup(Some(old_at), Some(new_at));
            assert_eq!(undo(&swap), Ok(outcome), "old at {old_at:?}, new at {new_at:?}");
            assert_eq!((content(&target), dir_at(&target)), (Some("old".into()), swap.old));
            assert!(!fresh.exists() && !aside.exists());
        }
        // Nothing was there: the copy is taken out again, or was never put in.
        for (new_at, outcome) in [(&fresh, "unchanged"), (&target, "rolled_back")] {
            let swap = setup(None, Some(new_at));
            assert_eq!(undo(&swap), Ok(outcome));
            assert!(!target.exists() && !fresh.exists());
        }
        // Something else at the place: left as it is, the old data's place said.
        let swap = setup(Some(&fresh), Some(&aside));
        write(&target.join("file"), "other");
        let why = undo(&swap).unwrap_err();
        assert!(why.contains(&format!("left at {}", fresh.display())), "{why}");
        assert_eq!(content(&target), Some("other".into()));
        assert_eq!(content(&fresh), Some("old".into()));
    }

    #[test]
    fn restored_data_carries_the_init_state_it_was_saved_with() {
        let id = DirIdentity { location: "/d".into(), identity: [1, 2] };
        let snap = |init: InitState, digest: Option<&str>, resume: Option<Resume>| SnapService {
            preset: "postgres".into(),
            version: "17".into(),
            major: "17".into(),
            files: 0,
            bytes: 0,
            init,
            init_digest: digest.map(String::from),
            init_resume: resume,
        };
        let done = restored_marker("s", &snap(InitState::Done, Some("d"), None), id.clone()).unwrap();
        assert_eq!((done.status.as_str(), done.digest.as_str(), done.data_dir.as_ref()), ("done", "d", Some(&id)));
        let task = |t: &str| init_tasks::CompletedTask { task: t.into(), digest: format!("{t}-digest") };
        let resume = Resume { digest: "d".into(), completed: vec![task("migrate")], failed_task: Some("seed".into()) };
        let failed = restored_marker("s", &snap(InitState::Failed, None, Some(resume)), id.clone()).unwrap();
        assert_eq!((failed.status.as_str(), failed.completed.len(), failed.failed_task.as_deref()), ("failed", 1, Some("seed")));
        // Nothing completed, or never begun: no marker, so init runs from the start.
        let none = Resume { digest: "d".into(), completed: Vec::new(), failed_task: Some("migrate".into()) };
        assert!(restored_marker("s", &snap(InitState::Failed, None, Some(none)), id.clone()).is_none());
        assert!(restored_marker("s", &snap(InitState::Pending, None, None), id.clone()).is_none());
        // A snapshot an earlier Stack wrote has no `init_resume`; one this Stack writes is read
        // by an earlier Stack as having no digest (init from the start), never as done.
        let old: SnapService = serde_json::from_value(json!({ "preset": "postgres", "version": "17", "major": "17", "files": 0, "bytes": 0, "init": "failed" })).unwrap();
        assert!(old.init_resume.is_none() && old.init_digest.is_none());
    }

    /// A complete snapshot directory of `name`, created at `created_at`, holding `data`.
    fn snapshot_dir(path: &Path, name: &str, created_at: u64, data: &str) {
        let meta = Meta { name: name.into(), format: 1, project: "p".into(), created_at, source: "/src".into(), services: IndexMap::new() };
        write(&path.join("data/db/PG_VERSION"), data);
        write(&path.join(META), &serde_json::to_string(&meta).unwrap());
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        out.sort();
        out
    }

    /// The name of a generation set aside by a `save --force` whose process is gone (an exited
    /// child's pid: on Linux pid 1 always runs, so a fixed pid would be an owner still alive).
    fn crashed(nanos: u64) -> String {
        static GONE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let pid = *GONE.get_or_init(|| {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            pid
        });
        format!(".old-base-{pid}-{nanos}")
    }

    #[test]
    fn staging_names_say_their_kind_snapshot_and_owner() {
        let base = Path::new("/s");
        let tmp = unique(base, ".tmp-a-b.c");
        let parsed = Staging::parse(tmp.file_name().unwrap().to_str().unwrap()).unwrap();
        assert_eq!((parsed.kind, parsed.name.as_str(), parsed.pid), ("tmp", "a-b.c", std::process::id()));
        assert_eq!(Staging::parse(".old-x-12-34"), Some(Staging { kind: "old", name: "x".into(), pid: 12, nanos: 34 }));
        assert_eq!(Staging::parse(".rm-x-12-34").unwrap().kind, "rm");
        for not in ["x", ".tmp-x", ".tmp--1-2", ".tmp-x-a-2", ".new-x-1-2", ".db.stack-old-1-2"] {
            assert_eq!(Staging::parse(not), None, "{not}");
        }
        assert!(!parsed.live_elsewhere(), "this process's own staging is stale under the lock");
    }

    #[test]
    fn a_forced_save_replaces_the_published_generation_whole() {
        for no_exchange in [false, true] {
            datadirs::NO_EXCHANGE.with(|c| c.set(no_exchange));
            let state = tempfile::tempdir().unwrap();
            let project = ProjectId::checkout(Path::new("/checkout"));
            let base = store(state.path(), &project);
            snapshot_dir(&base.join("base"), "base", 1, "old");
            let tmp = unique(&base, ".tmp-base");
            snapshot_dir(&tmp, "base", 2, "new");
            let how = publish(&base, "base", &tmp, &base.join("base")).unwrap();
            assert_eq!(how, if no_exchange { "rename" } else { "exchange" });
            assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "new");
            assert_eq!(entries(&base), ["base"], "nothing staged is left");
            assert_eq!(read_meta(state.path(), &project, "base").unwrap().created_at, 2);
        }
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
    }

    #[test]
    fn a_save_interrupted_between_its_renames_keeps_the_old_generation_readable_and_recovers_it() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        snapshot_dir(&base.join("base"), "base", 1, "old");
        snapshot_dir(&base.join("other"), "other", 1, "x");
        let tmp = unique(&base, ".tmp-base");
        snapshot_dir(&tmp, "base", 2, "new");
        datadirs::NO_EXCHANGE.with(|c| c.set(true));
        // As a crash between the renames leaves it: the new one never published, the old one
        // never put back.
        FAIL_RENAMES.with(|c| c.set(&["publish", "put_back"]));
        let e = publish(&base, "base", &tmp, &base.join("base")).unwrap_err();
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
        FAIL_RENAMES.with(|c| c.set(&[]));
        assert!(e.message.contains("the next snapshot command puts it back"), "{e}");
        assert!(!base.join("base").exists(), "crashed with the name empty");

        // Readers without the lock see the old generation, never a missing snapshot.
        assert_eq!(names(state.path(), &project), ["base", "other"]);
        let meta = read_meta(state.path(), &project, "base").unwrap();
        assert_eq!(meta.created_at, 1);
        assert!(generation(state.path(), &project, "base").unwrap().unwrap().file_name().unwrap().to_str().unwrap().starts_with(".old-base-"));

        // A stale staging directory of a process that is gone, and a live one of another.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let stale = base.join(format!(".tmp-other-{gone_pid}-5"));
        snapshot_dir(&stale, "other", 9, "half");
        let mut alive = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let live = base.join(format!(".tmp-other-{}-6", alive.id()));
        fs::create_dir_all(&live).unwrap();

        let recovered = recover_store(&base).unwrap();
        assert_eq!(recovered.len(), 1, "{recovered:?}");
        assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "old", "the last valid generation is back");
        let mut want = vec!["base".to_string(), "other".to_string(), live.file_name().unwrap().to_str().unwrap().to_string()];
        want.sort();
        assert_eq!(entries(&base), want, "staging nobody owns is removed; a live owner's is left alone");
        alive.kill().unwrap();
        alive.wait().unwrap();
        // Nothing to recover: a published generation is never replaced by one set aside.
        snapshot_dir(&unique(&base, ".old-base"), "base", 7, "older");
        assert!(recover_store(&base).unwrap().is_empty());
        assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "old");
        assert_eq!(entries(&base), ["base", "other"]);
    }

    #[test]
    fn recovery_takes_the_newest_valid_generation_and_ignores_invalid_ones() {
        let state = tempfile::tempdir().unwrap();
        let base = state.path().join("store");
        snapshot_dir(&base.join(crashed(10)), "base", 3, "three");
        snapshot_dir(&base.join(crashed(20)), "base", 2, "two");
        // Names another snapshot, and one with no metadata: neither is taken.
        snapshot_dir(&base.join(crashed(30)), "elsewhere", 9, "nine");
        write(&base.join(crashed(40)).join("data/db/PG_VERSION"), "partial");
        // An incomplete directory at the name (no meta.json) is set aside too.
        write(&base.join("base/data/db/PG_VERSION"), "partial");
        recover_store(&base).unwrap();
        assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "three");
        assert_eq!(entries(&base), ["base"]);
    }

    /// The store's staging directories of one kind (`old`, `rm`, `tmp`).
    fn staged(base: &Path, kind: &str) -> Vec<PathBuf> {
        staging(base).unwrap().into_iter().filter(|(_, s)| s.kind == kind).map(|(p, _)| p).collect()
    }

    #[test]
    fn a_published_generation_whose_metadata_is_malformed_never_hides_or_removes_the_valid_one() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        snapshot_dir(&base.join(crashed(10)), "base", 3, "valid");
        for published in ["{ not json", r#"{"name":"elsewhere","format":1,"project":"p","created_at":9,"source":"/s","services":{}}"#] {
            let _ = fs::remove_dir_all(base.join("base"));
            write(&base.join("base/data/db/PG_VERSION"), "damaged");
            write(&base.join("base").join(META), published);
            // Readers take the valid generation set aside, not the damaged one at the name.
            assert!(generation(state.path(), &project, "base").unwrap().unwrap().ends_with(crashed(10)), "{published}");
            assert_eq!(read_meta(state.path(), &project, "base").unwrap().created_at, 3);
            assert_eq!(list_names(state.path(), &project), ["base"]);
            // Recovery puts the valid one in its place, never removes it.
            let recovered = recover_store(&base).unwrap();
            assert_eq!(recovered.len(), 1, "{recovered:?}");
            assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "valid");
            assert_eq!(entries(&base), ["base"]);
            // Set aside again for the next round.
            fs::rename(base.join("base"), base.join(crashed(10))).unwrap();
        }
        // Damaged with nothing valid set aside: reported, and left for `rm`, never removed.
        fs::remove_dir_all(base.join(crashed(10))).unwrap();
        write(&base.join("base").join(META), "{ not json");
        snapshot_dir(&base.join(crashed(20)), "elsewhere", 1, "other");
        assert_eq!(read_meta(state.path(), &project, "base").unwrap_err().code, "snapshot_invalid");
        assert!(recover_store(&base).unwrap().is_empty());
        assert_eq!(entries(&base), ["base"], "the invalid generation set aside is staging; the name is left for `rm`");
    }

    /// Readers take no lock, and a `save --force` without an exchange moves the published
    /// generation aside, publishes the new one and removes the old one between any two of their
    /// reads: every read, of the metadata, the listing and `list`, still finds a generation.
    #[test]
    fn readers_racing_forced_saves_without_an_exchange_always_find_a_generation() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        snapshot_dir(&base.join("base"), "base", 1, "x");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saver = {
            let (base, stop) = (base.clone(), stop.clone());
            std::thread::spawn(move || {
                datadirs::NO_EXCHANGE.with(|c| c.set(true));
                let mut created_at = 1;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    created_at += 1;
                    let tmp = unique(&base, ".tmp-base");
                    snapshot_dir(&tmp, "base", created_at, "x");
                    assert_eq!(publish(&base, "base", &tmp, &base.join("base")).unwrap(), "rename");
                }
                created_at
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let (mut reads, mut misses) = (0, Vec::new());
        while std::time::Instant::now() < deadline {
            reads += 1;
            if let Err(e) = read_meta(state.path(), &project, "base") {
                misses.push(format!("read_meta: {}", e.code));
            }
            if names(state.path(), &project) != ["base"] {
                misses.push("names".into());
            }
            if list_names(state.path(), &project) != ["base"] {
                misses.push("generation".into());
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let saves = saver.join().unwrap();
        assert!(saves > 10, "the saver published only {saves} generations");
        assert!(misses.is_empty(), "{} of {reads} rounds missed the snapshot during {saves} saves: {:?}", misses.len(), &misses[..misses.len().min(10)]);
    }

    /// The interleaving behind the race above, one step at a time: the reader finds the name
    /// empty (moved aside) and the generation set aside already half removed, and only then is
    /// the new one published. It reads the name again and finds that.
    #[test]
    fn a_reader_that_finds_neither_the_name_nor_a_generation_set_aside_reads_the_name_again() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        // A save midway: the old generation renamed aside and its metadata already removed, the
        // new one built but not yet published.
        let old = unique(&base, ".old-base");
        snapshot_dir(&old, "base", 1, "x");
        fs::remove_file(old.join(META)).unwrap();
        let tmp = unique(&base, ".tmp-base");
        snapshot_dir(&tmp, "base", 2, "x");
        let (published, aside) = (base.join("base"), old.clone());
        let mut steps = 0;
        BETWEEN_READS.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                steps += 1;
                if steps == 1 {
                    fs::rename(&tmp, &published).unwrap();
                    remove_tree(&aside);
                }
            }))
        });
        let meta = read_meta(state.path(), &project, "base");
        BETWEEN_READS.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(meta.unwrap().created_at, 2);

        // With no save there, a missing snapshot is still missing, after a bounded look.
        fs::remove_dir_all(base.join("base")).unwrap();
        assert_eq!(read_meta(state.path(), &project, "base").unwrap_err().code, "snapshot_not_found");
    }

    fn list_names(state: &Path, project: &ProjectId) -> Vec<String> {
        names(state, project).into_iter().filter(|n| generation(state, project, n).unwrap().is_some()).collect()
    }

    #[test]
    fn a_failed_rename_of_a_forced_save_keeps_the_published_generation() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        datadirs::NO_EXCHANGE.with(|c| c.set(true));
        // The first rename (the published one aside) fails: nothing moved.
        // The second (the new one in) fails: the published one is put back.
        for failing in [&["aside"][..], &["publish"][..]] {
            let _ = fs::remove_dir_all(&base);
            snapshot_dir(&base.join("base"), "base", 1, "old");
            let tmp = unique(&base, ".tmp-base");
            snapshot_dir(&tmp, "base", 2, "new");
            FAIL_RENAMES.with(|c| c.set(failing));
            let e = publish(&base, "base", &tmp, &base.join("base")).unwrap_err();
            FAIL_RENAMES.with(|c| c.set(&[]));
            assert!(e.message.contains("failed (test)"), "{failing:?}: {e}");
            assert_eq!(read_meta(state.path(), &project, "base").unwrap().created_at, 1, "{failing:?}");
            assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "old", "{failing:?}");
            assert!(staged(&base, "old").is_empty(), "{failing:?}: nothing is left set aside");
        }
        datadirs::NO_EXCHANGE.with(|c| c.set(false));
    }

    #[test]
    fn a_failed_rename_of_recovery_keeps_the_generation_set_aside() {
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        // Moving the incomplete directory at the name out of the way fails, then putting the
        // generation set aside in its place does.
        for failing in [&["recover_stray"][..], &["recover_old"][..]] {
            let _ = fs::remove_dir_all(&base);
            snapshot_dir(&base.join(crashed(10)), "base", 3, "valid");
            write(&base.join("base/data/db/PG_VERSION"), "partial");
            FAIL_RENAMES.with(|c| c.set(failing));
            let e = recover_store(&base).unwrap_err();
            FAIL_RENAMES.with(|c| c.set(&[]));
            assert_eq!(e.code, "snapshot_failed", "{failing:?}: {e}");
            assert!(e.message.contains(&crashed(10)), "{failing:?}: says where it is kept: {e}");
            let old = staged(&base, "old");
            assert_eq!(old.len(), 1, "{failing:?}: the generation set aside is never removed");
            assert_eq!(fs::read_to_string(old[0].join("data/db/PG_VERSION")).unwrap(), "valid");
            // Readers still find it, and the next recovery puts it back.
            assert_eq!(read_meta(state.path(), &project, "base").unwrap().created_at, 3, "{failing:?}");
            assert_eq!(recover_store(&base).unwrap().len(), 1, "{failing:?}");
            assert_eq!(fs::read_to_string(base.join("base/data/db/PG_VERSION")).unwrap(), "valid");
            assert_eq!(entries(&base), ["base"], "{failing:?}");
        }
    }

    #[test]
    fn a_store_that_cannot_be_read_is_an_error_not_an_empty_store() {
        use std::os::unix::fs::PermissionsExt;
        let state = tempfile::tempdir().unwrap();
        let project = ProjectId::checkout(Path::new("/checkout"));
        let base = store(state.path(), &project);
        snapshot_dir(&base.join(crashed(10)), "base", 3, "valid");
        let old = base.join(crashed(10));
        fs::set_permissions(&old, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway: nothing to show then.
        let unreadable = fs::read_to_string(old.join(META)).is_err();
        let recovered = recover_store(&base);
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
        if unreadable {
            assert!(recovered.is_err(), "{recovered:?}");
            assert_eq!(staged(&base, "old").len(), 1, "never removed on a read that failed");
        }
    }

    #[test]
    fn names_are_plain_and_majors_are_the_leading_component() {
        for ok in ["base", "migrated.v2", "a_b-c"] {
            valid_name(ok).unwrap();
        }
        for bad in ["", ".hidden", "../x", "a/b", "-x", &"x".repeat(65)] {
            assert_eq!(valid_name(bad).unwrap_err().code, "usage", "{bad}");
        }
        assert_eq!(major("17.11"), "17");
        assert_eq!(major("8.2.1"), "8");
        assert_eq!(major("17"), "17");
    }

    #[test]
    fn worktrees_of_one_repository_share_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str], at: &Path| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(at)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["commit", "-q", "--allow-empty", "-m", "x"], &repo);
        let wt = dir.path().join("wt");
        git(&["worktree", "add", "-q", wt.to_str().unwrap()], &repo);
        let (a, b) = (project_id(&repo.canonicalize().unwrap()), project_id(&wt.canonicalize().unwrap()));
        assert_eq!((a.kind, a.id.clone()), ("git", b.id.clone()));
        let sub = repo.join("svc");
        fs::create_dir(&sub).unwrap();
        assert_ne!(project_id(&sub.canonicalize().unwrap()).id, a.id, "another project in the same repository");
        let lone = dir.path().join("lone");
        fs::create_dir(&lone).unwrap();
        assert_eq!(project_id(&lone.canonicalize().unwrap()).kind, "checkout");
        // Read from Git's files, the identity is the same.
        for at in [&repo, &wt, &sub, &lone] {
            let at = at.canonicalize().unwrap();
            let (asked, read) = (project_id(&at), project_id_offline(&at));
            assert_eq!((asked.id, asked.kind, asked.subdir), (read.id, read.kind, read.subdir), "{at:?}");
        }
    }
}
