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
use crate::state::{now, read_json, write_json, FileLock};
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
        Some((common, sub)) => ProjectId {
            id: sha256_hex(format!("git\0{}\0{sub}", common.display()).as_bytes())[..16].to_string(),
            kind: "git",
            git_common_dir: Some(common),
            subdir: Some(sub),
            checkout: None,
        },
        None => ProjectId {
            id: sha256_hex(format!("checkout\0{}", root.display()).as_bytes())[..16].to_string(),
            kind: "checkout",
            git_common_dir: None,
            subdir: None,
            checkout: Some(root.to_path_buf()),
        },
    }
}

fn store(state: &Path, project: &ProjectId) -> PathBuf {
    state.join("snapshots").join(&project.id)
}

fn store_lock(state: &Path, project: &ProjectId) -> Result<FileLock> {
    FileLock::acquire(&state.join("snapshots").join(format!("{}.lock", project.id)))
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

fn read_meta(state: &Path, project: &ProjectId, name: &str) -> Result<Meta> {
    let path = store(state, project).join(name).join(META);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| StackError::new("snapshot_invalid", format!("{}: {e}", path.display())).hint(format!("remove it with `stack snapshot rm {name}`"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_found(state, project, name)),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

fn not_found(state: &Path, project: &ProjectId, name: &str) -> StackError {
    let known = names(state, project);
    StackError::new("snapshot_not_found", format!("no snapshot named '{name}' for this project")).hint(if known.is_empty() {
        "save one with `stack snapshot save <name>` in a checkout whose database is ready".to_string()
    } else {
        format!("snapshots: {} (`stack snapshot list`)", known.join(", "))
    })
}

fn names(state: &Path, project: &ProjectId) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(store(state, project))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(META).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_name(n).is_ok())
        .collect();
    out.sort();
    out
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
            Ok(DataDir { dir, location, exists: true, token })
        }
        // Nothing there yet: it may be created, at the place the record says.
        Found::Gone | Found::Unbound(_) if absent => {
            let location = datadirs::locate(&dir).0.filter(|l| entry.location.as_ref().map_or(true, |r| r == l)).ok_or_else(|| {
                failed(format!("{} no longer resolves where Stack recorded it", dir.display())).hint(rerun)
            })?;
            if !marked(fs::read(session::data_marker(&dir)).ok()) {
                return Err(failed(format!("{} does not carry this checkout's marker", dir.display())).hint(rerun));
            }
            Ok(DataDir { dir, location, exists: false, token })
        }
        Found::Gone | Found::Unbound(_) => Err(failed(format!("{} is not the data directory Stack recorded for '{service}'", dir.display())).hint(rerun)),
        Found::Other(why) => Err(StackError::new("snapshot_unsupported", format!("{service}'s data directory: {why}")).hint("Stack copies only the data directory it recorded, reached with no link followed")),
    }
}

/// After a restore put another directory at `data`'s place: the record names the new one.
fn rebind(ctx: &Ctx, data: &DataDir) -> Result<()> {
    datadirs::bind(&ctx.state, &data.dir, &data.token)
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

fn unique(parent: &Path, prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    parent.join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn remove_tree(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Exchange two existing paths in one step, where the platform can.
fn exchange(a: &Path, b: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let (Ok(a), Ok(b)) = (std::ffi::CString::new(a.as_os_str().as_bytes()), std::ffi::CString::new(b.as_os_str().as_bytes())) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        // SAFETY: valid NUL-terminated paths.
        unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) == 0 }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: valid NUL-terminated paths; renameat2 reads them only.
        unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_EXCHANGE) == 0 }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (a, b);
        false
    }
}

/// Put the complete copy `fresh` (a sibling of `target`) at `target`. With no `target` yet,
/// one rename. Otherwise an exchange, or two renames where the platform has none: `target`
/// is then briefly missing, never half-written, and the old data is kept aside until the new
/// is in place.
fn replace(fresh: &Path, target: &Path) -> Result<&'static str> {
    if fs::symlink_metadata(target).is_err() {
        fs::rename(fresh, target).map_err(|e| io_error(target.display(), e))?;
        return Ok("rename");
    }
    if exchange(fresh, target) {
        remove_tree(fresh);
        return Ok("exchange");
    }
    let parent = target.parent().unwrap_or(Path::new("/"));
    let aside = unique(parent, &format!(".{}.stack-old", target.file_name().unwrap_or_default().to_string_lossy()));
    fs::rename(target, &aside).map_err(|e| io_error(target.display(), e))?;
    if let Err(e) = fs::rename(fresh, target) {
        let _ = fs::rename(&aside, target);
        return Err(io_error(target.display(), e));
    }
    remove_tree(&aside);
    Ok("rename")
}

/// Copy a snapshot's data for `service` to `target`, through a sibling temporary directory.
fn restore_into(snapshot: &Path, service: &str, target: &Path) -> Result<Value> {
    let parent = target.parent().expect("checked absolute with a parent");
    fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    let fresh = unique(parent, &format!(".{}.stack-restore", target.file_name().unwrap_or_default().to_string_lossy()));
    let copied = copy_tree(&snapshot.join("data").join(service), &fresh);
    let how = match copied.and_then(|how| replace(&fresh, target).map(|swap| (how, swap))) {
        Ok(how) => how,
        Err(e) => {
            remove_tree(&fresh);
            return Err(e);
        }
    };
    Ok(json!({ "service": service, "data_dir": target, "copy": how.0, "replace": how.1 }))
}

/// Mark restored data as initialised for the init digest the snapshot records, or forget any
/// marker when it records none.
fn mark_restored(ctx: &Ctx, name: &str, service: &str, snap: &SnapService, target: &Path) -> Result<()> {
    let marker = snap.init_digest.clone().map(|digest| Marker {
        data_dir: DirIdentity::of(target),
        status: "done".into(),
        digest,
        completed: Vec::new(),
        failed_task: None,
        source: format!("snapshot:{name}"),
        at: now(),
    });
    init_tasks::set_marker(&ctx.state, &ctx.root, service, marker)
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
    if final_dir.exists() && !force {
        return Err(StackError::new("snapshot_exists", format!("snapshot '{name}' already exists for this project"))
            .hint(format!("pick another name, `stack snapshot rm {name}`, or `stack snapshot save {name} --force` to replace it")));
    }
    let _run = init_tasks::run_lock(&ctx.state, &ctx.root)?;
    let stopped = session::with_stopped(ctx, &services, |report, statuses| {
        let _store = store_lock(&ctx.state, &project)?;
        let base = store(&ctx.state, &project);
        fs::create_dir_all(&base).map_err(|e| io_error(base.display(), e))?;
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
                let version = locked_version(report, service).unwrap_or_default();
                meta.services.insert(service.clone(), SnapService {
                    preset: preset(report, service).unwrap_or_default().to_string(),
                    major: major(&version),
                    version,
                    files: tally.files,
                    bytes: tally.bytes,
                    init,
                    init_digest,
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
        if final_dir.exists() {
            if !force {
                remove_tree(&tmp);
                return Err(StackError::new("snapshot_exists", format!("snapshot '{name}' was created while this one was saved")));
            }
            let old = unique(&base, &format!(".old-{name}"));
            fs::rename(&final_dir, &old).map_err(|e| io_error(final_dir.display(), e))?;
            fs::rename(&tmp, &final_dir).map_err(|e| io_error(final_dir.display(), e))?;
            remove_tree(&old);
        } else {
            fs::rename(&tmp, &final_dir).map_err(|e| io_error(final_dir.display(), e))?;
        }
        Ok(json!({ "copied": copied, "meta": meta }))
    })?;
    let meta = serde_json::from_value(stopped.result["meta"].clone()).ok();
    Ok(SnapshotReport { name: name.to_string(), project, services, steps: stopped.steps, restarted: stopped.restarted, checks: stopped.checks, meta })
}

pub fn restore(ctx: &Ctx, name: &str) -> Result<SnapshotReport> {
    valid_name(name)?;
    let project = project_id(&ctx.root);
    let meta = read_meta(&ctx.state, &project, name)?;
    compatible(&meta, &compile_readonly(ctx)?)?;
    let services: Vec<String> = meta.services.keys().cloned().collect();
    let _run = init_tasks::run_lock(&ctx.state, &ctx.root)?;
    let stopped = session::with_stopped(ctx, &services, |report, statuses| {
        let _store = store_lock(&ctx.state, &project)?;
        // Read again under the lock: it may have been replaced or removed meanwhile.
        let meta = read_meta(&ctx.state, &project, name)?;
        compatible(&meta, report)?;
        let snapshot = store(&ctx.state, &project).join(name);
        let mut restored = Vec::new();
        // Every directory is checked before any is replaced.
        let targets: Vec<DataDir> = meta.services.keys().map(|service| data_dir(ctx, statuses, service)).collect::<Result<_>>()?;
        for ((service, snap), data) in meta.services.iter().zip(&targets) {
            restored.push(restore_into(&snapshot, service, &data.location)?);
            rebind(ctx, data)?;
            mark_restored(ctx, name, service, snap, &data.location)?;
        }
        Ok(json!({ "restored": restored }))
    })?;
    Ok(SnapshotReport { name: name.to_string(), project, services, steps: stopped.steps, restarted: stopped.restarted, checks: stopped.checks, meta: Some(meta) })
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
    let meta = read_meta(&ctx.state, &project, name)?;
    compatible(&meta, report)?;
    let snapshot = store(&ctx.state, &project).join(name);
    let (mut restored, mut skipped) = (Vec::new(), Vec::new());
    for (service, snap) in &meta.services {
        let data = data_dir(ctx, statuses, service)?;
        if data.exists {
            skipped.push(json!({ "service": service, "data_dir": data.dir, "reason": "data directory exists" }));
            continue;
        }
        // Nothing can be running on a directory that does not exist; still, never copy under
        // a running daemon.
        if statuses.iter().any(|d| d.name == *service && matches!(d.status.as_str(), "running" | "starting" | "waiting")) {
            return Err(StackError::new("snapshot_failed", format!("'{service}' is running; nothing was restored")).hint("run `stack down`, then retry"));
        }
        restored.push(restore_into(&snapshot, service, &data.location)?);
        rebind(ctx, &data)?;
        mark_restored(ctx, name, service, snap, &data.location)?;
    }
    Ok(json!({ "snapshot": name, "restored": restored, "skipped": skipped }))
}

#[derive(Debug, Serialize)]
pub struct ListReport {
    pub project: ProjectId,
    pub snapshots: Vec<Meta>,
}

pub fn list(ctx: &Ctx) -> Result<ListReport> {
    let project = project_id(&ctx.root);
    let snapshots = names(&ctx.state, &project)
        .iter()
        .filter_map(|n| read_json::<Option<Meta>>(&store(&ctx.state, &project).join(n).join(META)).ok().flatten())
        .collect();
    Ok(ListReport { project, snapshots })
}

pub fn rm(ctx: &Ctx, name: &str) -> Result<Value> {
    valid_name(name)?;
    let project = project_id(&ctx.root);
    let _store = store_lock(&ctx.state, &project)?;
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

    #[test]
    fn replacing_swaps_whole_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("redis");
        write(&target.join("dump.rdb"), "old");
        let fresh = dir.path().join(".fresh");
        write(&fresh.join("dump.rdb"), "new");
        replace(&fresh, &target).unwrap();
        assert_eq!(fs::read_to_string(target.join("dump.rdb")).unwrap(), "new");
        assert!(!fresh.exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("redis")]);
        // Into a place that does not exist yet.
        let fresh = dir.path().join(".fresh2");
        write(&fresh.join("x"), "1");
        replace(&fresh, &dir.path().join("new")).unwrap();
        assert!(dir.path().join("new/x").exists());
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
    }
}
