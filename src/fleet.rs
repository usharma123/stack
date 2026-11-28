//! Machine-wide views and maintenance across checkouts: `ps` lists what Stack knows about,
//! `mv` follows a checkout that was moved, and the fleet forms of `status`, `logs` and `down`
//! act on every checkout a selection matches (one project's worktrees, a label, or all).
//!
//! Everything here reads the same machine state the lifecycle writes (`sessions/`, `ports.json`,
//! `identities.json`, `data.json`). `ps` only reads it, without any lock: every state file is
//! replaced atomically, so a read sees one version or the other, and `ps` never waits behind
//! `up`. What `ps` adds about each checkout (its project, branch and commit, registered
//! commands, changed `watch` files) is read from files too, never by running `git` per checkout.

use crate::checkout;
use crate::datadirs::{self, Found};
use crate::error::{io_error, Result, StackError};
use crate::gitdir::{self, GitDirs};
use crate::identity;
use crate::labels::{self, Labels};
use crate::lease::LeaseSummary;
use crate::manifest::{self, ProjectManifest};
use crate::ports;
use crate::session::{self, Ctx, DownReport, Session, StatusReport};
use crate::state::{now, pid_alive, project_key, project_lock, write_json, FileLock};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// What `ps` checks per service. Stated in every report so nobody reads it as verification.
const PS_CHECKS: &str = "liveness only: the recorded pid exists and the port accepts connections. \
    Not proof that it is this checkout's instance; run `stack status` in the checkout for that";

/// Every `state` a checkout can be listed in, as `ps --state` accepts them.
pub const STATES: &[&str] = &["running", "partial", "stopped", "launching", "recorded", "reserved", "unreadable", "unstarted"];

#[derive(Debug, Serialize)]
pub struct PsReport {
    /// How far the per-service fields go (liveness, never identity).
    pub checks: &'static str,
    pub checkouts: Vec<Checkout>,
    pub totals: Totals,
}

#[derive(Debug, Serialize)]
pub struct Checkout {
    pub path: PathBuf,
    pub dir_exists: bool,
    /// `present`, `missing`, `replaced` (another directory now has this path), or
    /// `unavailable` (whether it is still there cannot be shown: a lookup refused or failing, or
    /// its volume may not be mounted).
    pub dir: &'static str,
    /// `running` (every service live), `partial`, `stopped`, `launching`, `recorded` (a session
    /// without services), `reserved` (ports, tokens or data recorded, no session),
    /// `unreadable` (a session record that cannot be read), or `unstarted` (`ps --worktrees`: a
    /// worktree of the project with a stack.toml and nothing of Stack's recorded yet).
    pub state: &'static str,
    pub session: Option<SessionSummary>,
    pub lease: Option<LeaseSummary>,
    pub services: Vec<ServiceRow>,
    pub identity_tokens: usize,
    /// What `stack gc` reclaims for this checkout: its directory is gone and no session of it
    /// is left, so gc releases its ports and reports its recorded data, which `gc
    /// --reclaim-data` removes where it can confirm it is safe to.
    pub reclaimable: bool,
    /// Why its session record cannot be read (state `unreadable`): run `stack down` in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
    /// The project it is a checkout of, as snapshots share them: one id for every worktree of a
    /// repository at the same subdirectory (see [`crate::snapshot::project_id`]). Absent when
    /// the directory is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// The branch its worktree has checked out; absent when detached or not in Git.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit `HEAD` names, abbreviated to 7 characters, when it can be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The session's labels (`stack up --label`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Commands Stack started here (`exec`, `run`, MCP) still running, from the command
    /// registry alone (see [`crate::commands`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub commands: usize,
    /// Live services whose `watch` files (as this checkout's stack.toml lists them) changed
    /// since they started: `stack restart --changed` restarts them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_changed: Vec<String>,
    /// Port reservations of this checkout, for the totals.
    #[serde(skip)]
    ports_reserved: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub started_at: u64,
    pub launching: bool,
}

#[derive(Debug, Serialize)]
pub struct ServiceRow {
    pub name: String,
    /// The port the session recorded, else the reserved one.
    pub port: Option<u16>,
    /// The reservation, when it differs from the recorded port (configuration changed since `up`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_port: Option<u16>,
    pub pid: Option<u32>,
    /// Whether a process with the recorded pid exists (null without a recorded pid).
    pub pid_alive: Option<bool>,
    /// Whether something accepts TCP connections on `port` on 127.0.0.1. Possibly another program.
    pub port_accepting: bool,
    /// The service's data directory: as `up` recorded it in the machine's data record (the one
    /// `gc` reclaims from), else as the session recorded it.
    pub data_dir: Option<String>,
    /// Bytes `data_dir` occupies on disk; null when it is not recorded, missing, or `--no-size`.
    pub data_bytes: Option<u64>,
    /// Where to reach a live service of a session: its preset's connection URL (such as
    /// `postgresql://postgres@127.0.0.1:<port>/postgres` or `redis://127.0.0.1:<port>`), or
    /// `http://127.0.0.1:<port>` for a custom service. Liveness only, like the rest of the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Totals {
    pub checkouts: usize,
    pub running: usize,
    pub services: usize,
    /// Services whose pid is alive, or without a recorded pid whose port accepts connections.
    pub services_live: usize,
    pub ports_reserved: usize,
    pub data_bytes: u64,
    /// The part of `data_bytes` that belongs to reclaimable checkouts.
    pub reclaimable_bytes: u64,
}

/// Which checkouts `ps` lists. The default lists every checkout Stack has records for, as
/// `ps` always has.
#[derive(Debug, Clone, Default)]
pub struct PsOptions {
    /// Only checkouts of this directory's project (see [`Checkout::project_id`]): the worktrees
    /// of its repository at the same subdirectory, or the directory alone outside Git.
    pub project: Option<PathBuf>,
    /// Only checkouts in this state (one of [`STATES`]).
    pub state: Option<String>,
    /// Only checkouts whose session has every one of these labels.
    pub labels: Labels,
    /// Also list worktrees of the repository (of `project`, else of `cwd`) that have a
    /// stack.toml and nothing of Stack's recorded yet, as `unstarted`.
    pub worktrees: bool,
    /// The caller's checkout: where `worktrees` finds the repository without `project`.
    pub cwd: Option<PathBuf>,
}

impl PsOptions {
    fn check(&self) -> Result<()> {
        match self.state.as_deref() {
            Some(state) if !STATES.contains(&state) => Err(StackError::new("usage", format!("unknown checkout state {state:?}"))
                .hint(format!("give one of: {}", STATES.join(", ")))),
            _ => Ok(()),
        }
    }
}

/// Every checkout with a session record, port reservations, identity tokens or recorded data.
pub fn ps(state: &Path, sizes: bool) -> Result<PsReport> {
    ps_with(state, sizes, &PsOptions::default())
}

/// What Stack recorded on this machine, read once without a lock.
struct Records {
    sessions: BTreeMap<PathBuf, Session>,
    unreadable: IndexMap<PathBuf, String>,
    reservations: Vec<ports::Reservation>,
    tokens: Vec<(PathBuf, String)>,
    data: Vec<datadirs::Entry>,
}

impl Records {
    fn read(state: &Path) -> Result<Self> {
        let (indexed, unreadable) = indexed_sessions(state)?;
        let sessions = indexed.into_iter().map(|s| (s.project.clone(), s)).collect();
        Ok(Self { sessions, unreadable, reservations: ports::all(state)?, tokens: identity::holders(state)?, data: datadirs::entries(state)? })
    }

    /// Every checkout path anything is recorded for, sorted.
    fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.sessions.keys().cloned().collect();
        paths.extend(self.reservations.iter().map(|r| r.project.clone()));
        paths.extend(self.tokens.iter().map(|(p, _)| p.clone()));
        paths.extend(self.data.iter().map(|e| e.project.clone()));
        paths.extend(self.unreadable.keys().cloned());
        paths.sort();
        paths.dedup();
        paths
    }

    /// `present`, `missing`, `replaced`, or `unavailable` (whether it is still there cannot be
    /// shown: see [`crate::checkout::presence`]), as `ps` reports a checkout's directory.
    fn dir(&self, path: &Path) -> &'static str {
        let recorded: Vec<&datadirs::Entry> = self.data.iter().filter(|e| e.project == path).collect();
        if let Some(s) = self.sessions.get(path) {
            return match session::checkout_state(s) {
                session::CheckoutState::Present => "present",
                session::CheckoutState::Gone("project directory deleted") => "missing",
                session::CheckoutState::Gone(_) => "replaced",
                session::CheckoutState::Unavailable(_) => "unavailable",
            };
        }
        // The fullest record kept: one with a location, else one with a device.
        let mut kept: Vec<&crate::checkout::Recorded> = self.reservations.iter().filter(|r| r.project == path).map(|r| &r.project_at).chain(recorded.iter().map(|e| &e.project_at)).collect();
        kept.sort_by_key(|at| (at.location.is_none(), at.device.is_none()));
        let at = kept.first().copied().cloned().unwrap_or_default();
        let present = match crate::checkout::presence(path, &at) {
            crate::checkout::Presence::Present => true,
            crate::checkout::Presence::Absent => false,
            crate::checkout::Presence::Unknown(_) => return "unavailable",
        };
        match (present, self.sessions.get(path)) {
            (false, _) => "missing",
            // Only data recorded for a checkout token the directory there now does not have.
            (true, None) if !recorded.is_empty() && {
                let here = crate::checkout::token(path);
                recorded.iter().all(|e| here.as_deref() != Some(e.checkout.as_str()))
            } => "replaced",
            _ => "present",
        }
    }

    /// One checkout as `ps` lists it, its data not yet measured.
    fn checkout(&self, state: &Path, path: PathBuf, git: Option<&GitDirs>, project_id: Option<String>) -> Checkout {
        let session = self.sessions.get(&path);
        let reserved: Vec<&ports::Reservation> = self.reservations.iter().filter(|r| r.project == path).collect();
        let recorded: Vec<&datadirs::Entry> = self.data.iter().filter(|e| e.project == path).collect();
        let dir_exists = path.exists();
        let dir = self.dir(&path);
        let mut rows: IndexMap<String, ServiceRow> = IndexMap::new();
        if let Some(s) = session {
            for (name, record) in &s.services {
                rows.insert(name.clone(), ServiceRow {
                    name: name.clone(),
                    port: (record.port != 0).then_some(record.port),
                    reserved_port: None,
                    pid: record.pid,
                    pid_alive: record.pid.map(pid_alive),
                    port_accepting: false,
                    data_dir: record.data_dir.clone(),
                    data_bytes: None,
                    endpoint: None,
                });
            }
        }
        for r in &reserved {
            match rows.get_mut(&r.service) {
                Some(row) if row.port == Some(r.port) => {}
                Some(row) if row.port.is_none() => row.port = Some(r.port),
                Some(row) => row.reserved_port = Some(r.port),
                None => {
                    rows.insert(r.service.clone(), ServiceRow::empty(&r.service, Some(r.port)));
                }
            }
        }
        for e in &recorded {
            let row = rows.entry(e.service.clone()).or_insert_with(|| ServiceRow::empty(&e.service, None));
            row.data_dir = Some(e.dir.to_string_lossy().into_owned());
        }
        let mut services: Vec<ServiceRow> = rows
            .into_values()
            .map(|mut row| {
                row.port_accepting = row.port.is_some_and(accepting);
                row
            })
            .collect();
        let live_count = services.iter().filter(|r| r.live()).count();
        let unreadable_record = self.unreadable.get(&path).cloned();
        let state_name = match session {
            None if unreadable_record.is_some() => "unreadable",
            None => "reserved",
            Some(s) if s.launching => "launching",
            Some(s) if s.services.is_empty() => "recorded",
            Some(_) if live_count == services.len() => "running",
            Some(_) if live_count > 0 => "partial",
            Some(_) => "stopped",
        };
        // Where to reach live services, and which changed under them: from this checkout's own
        // stack.toml, read only when a session has a live service.
        let mut watch_changed = Vec::new();
        if let Some(s) = session.filter(|_| dir_exists && live_count > 0) {
            let manifest = manifest::read_project(&path).ok();
            for row in services.iter_mut().filter(|r| r.live()) {
                row.endpoint = row.port.and_then(|port| endpoint(manifest.as_ref(), &row.name, row.data_dir.as_deref(), port));
                let started = s.services.get(&row.name).and_then(|r| r.started_at_ms);
                let watch = manifest.as_ref().map(|m| watch_of(m, &row.name)).unwrap_or_default();
                if let (false, Some(started)) = (watch.is_empty(), started) {
                    if !session::changed_since(&path, &watch, started).0.is_empty() {
                        watch_changed.push(row.name.clone());
                    }
                }
            }
        }
        let head = git.map(gitdir::head).unwrap_or_default();
        let commands = if dir_exists { crate::commands::listed(state, &path).map_or(0, |(c, _)| c.len()) } else { 0 };
        Checkout {
            dir_exists,
            dir,
            state: state_name,
            session: session.map(|s| SessionSummary { id: s.id.clone(), started_at: s.started_at, launching: s.launching }),
            lease: session.map(|s| LeaseSummary::of(s.lease.as_ref())),
            services,
            identity_tokens: self.tokens.iter().filter(|(p, _)| *p == path).count(),
            // As `gc` decides: a directory shown to be gone with no session left (an unreadable
            // one is left, and so is one whose presence cannot be shown).
            reclaimable: dir == "missing" && session.is_none() && unreadable_record.is_none(),
            unreadable: unreadable_record,
            project_id,
            branch: head.branch,
            head: head.commit,
            labels: session.map(|s| s.labels.clone()).unwrap_or_default(),
            commands,
            watch_changed,
            ports_reserved: reserved.len(),
            path,
        }
    }
}

/// [`ps`], listing only the checkouts `opts` selects (see [`PsOptions`]).
pub fn ps_with(state: &Path, sizes: bool, opts: &PsOptions) -> Result<PsReport> {
    opts.check()?;
    let records = Records::read(state)?;
    let scope = opts.project.as_deref().map(|p| crate::snapshot::project_id_offline(p).id);
    let paths = records.paths();
    let mut checkouts = Vec::new();
    for path in &paths {
        // Selections that need no liveness check are made first.
        let session = records.sessions.get(path);
        if !opts.labels.is_empty() && !session.is_some_and(|s| labels::matches(&s.labels, &opts.labels)) {
            continue;
        }
        let git = path.exists().then(|| gitdir::locate(path)).flatten();
        let project_id = path.exists().then(|| crate::snapshot::project_id_of(path, git.as_ref()).id);
        if scope.is_some() && project_id != scope {
            continue;
        }
        let mut checkout = records.checkout(state, path.clone(), git.as_ref(), project_id);
        if opts.state.as_deref().is_some_and(|s| s != checkout.state) {
            continue;
        }
        if sizes {
            for row in &mut checkout.services {
                row.data_bytes = row.data_dir.as_deref().and_then(|d| disk_usage(Path::new(d)));
            }
        }
        checkouts.push(checkout);
    }
    let unstarted_wanted = opts.worktrees && opts.labels.is_empty() && opts.state.as_deref().is_none_or(|s| s == "unstarted");
    if unstarted_wanted {
        if let Some(base) = opts.project.as_deref().or(opts.cwd.as_deref()) {
            for path in unstarted_worktrees(base, &paths) {
                let git = gitdir::locate(&path);
                let project_id = crate::snapshot::project_id_of(&path, git.as_ref()).id;
                if scope.as_ref().is_some_and(|s| *s != project_id) {
                    continue;
                }
                checkouts.push(Checkout::unstarted(state, path, git.as_ref(), project_id));
            }
        }
    }
    checkouts.sort_by(|a, b| a.path.cmp(&b.path));
    let mut totals = Totals::default();
    for c in &checkouts {
        let bytes: u64 = c.services.iter().filter_map(|r| r.data_bytes).sum();
        totals.checkouts += 1;
        totals.running += usize::from(c.state == "running");
        if c.session.is_some() {
            totals.services += c.services.len();
            totals.services_live += c.services.iter().filter(|r| r.live()).count();
        }
        totals.ports_reserved += c.ports_reserved;
        totals.data_bytes += bytes;
        if c.reclaimable {
            totals.reclaimable_bytes += bytes;
        }
    }
    Ok(PsReport { checks: PS_CHECKS, checkouts, totals })
}

/// Worktrees of the repository holding `base` that have a stack.toml where `base` has its
/// project (the same subdirectory) and that nothing in `recorded` names: one `git worktree
/// list` for the repository, and nothing at all outside Git.
fn unstarted_worktrees(base: &Path, recorded: &[PathBuf]) -> Vec<PathBuf> {
    let Some(dirs) = gitdir::locate(base) else { return Vec::new() };
    let Ok(sub) = base.strip_prefix(&dirs.toplevel) else { return Vec::new() };
    let mut out = Vec::new();
    for worktree in gitdir::worktrees(base).into_iter().filter(|w| !w.bare) {
        let Ok(top) = worktree.path.canonicalize() else { continue };
        let path = if sub.as_os_str().is_empty() { top } else { top.join(sub) };
        if path.join(manifest::PROJECT_FILE).is_file() && !recorded.contains(&path) && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

impl Checkout {
    fn unstarted(state: &Path, path: PathBuf, git: Option<&GitDirs>, project_id: String) -> Self {
        let head = git.map(gitdir::head).unwrap_or_default();
        Checkout {
            dir_exists: true,
            dir: "present",
            state: "unstarted",
            session: None,
            lease: None,
            services: Vec::new(),
            identity_tokens: 0,
            reclaimable: false,
            unreadable: None,
            project_id: Some(project_id),
            branch: head.branch,
            head: head.commit,
            labels: BTreeMap::new(),
            commands: crate::commands::listed(state, &path).map_or(0, |(c, _)| c.len()),
            watch_changed: Vec::new(),
            ports_reserved: 0,
            path,
        }
    }
}

impl ServiceRow {
    fn empty(name: &str, port: Option<u16>) -> Self {
        Self { name: name.to_string(), port, reserved_port: None, pid: None, pid_alive: None, port_accepting: false, data_dir: None, data_bytes: None, endpoint: None }
    }

    /// Its pid is alive, or without a recorded pid its port accepts connections.
    fn live(&self) -> bool {
        self.pid_alive.unwrap_or(self.port_accepting)
    }
}

/// The service table of `name` in a project's stack.toml: its override, else its definition.
fn service_of<'a>(manifest: &'a ProjectManifest, name: &str) -> impl Iterator<Item = &'a manifest::Service> {
    manifest.overrides.services.get(name).into_iter().chain(manifest.services.get(name))
}

/// The `watch` paths stack.toml gives `name` (empty for a service a bundle defines).
/// A full override (one with `preset` or `run`) replaces the base definition, including an empty
/// `watch` list.
fn watch_of(manifest: &ProjectManifest, name: &str) -> Vec<String> {
    if let Some(ov) = manifest.overrides.services.get(name) {
        if ov.preset.is_some() || ov.run.is_some() || !ov.watch.is_empty() {
            return ov.watch.clone();
        }
    }
    manifest.services.get(name).map(|s| s.watch.clone()).unwrap_or_default()
}

/// Where to reach the live service `name` on `port`: its preset's connection URL, the preset
/// told by stack.toml or else by the version file its data directory holds (every preset keeps
/// data, so a running one has it); `http://` for anything else, which is a custom service.
fn endpoint(manifest: Option<&ProjectManifest>, name: &str, data_dir: Option<&str>, port: u16) -> Option<String> {
    let defined = manifest.into_iter().flat_map(|m| service_of(m, name)).find(|s| s.preset.is_some() || s.run.is_some());
    let preset = match defined {
        Some(s) => s.preset.clone(),
        None => data_dir
            .and_then(|d| fs::read_to_string(Path::new(d).join(crate::supervisor::VERSION_FILE)).ok())
            .and_then(|text| text.trim().split(':').next().map(String::from))
            .filter(|p| !p.is_empty()),
    };
    match preset {
        None => Some(format!("http://127.0.0.1:{port}")),
        Some(preset) => preset_endpoint(&preset, port),
    }
}

/// A preset's connection URL on `port`, as the preset exports it (its `*_URL`, else its
/// `*_ENDPOINT`); `None` for a preset Stack does not know.
fn preset_endpoint(preset: &str, port: u16) -> Option<String> {
    let exports = crate::supervisor::preset_exports(preset, port, &crate::supervisor::Secondary::new());
    let by_suffix = |suffix: &str| exports.iter().find(|(k, _)| k.ends_with(suffix)).map(|(_, v)| v.clone());
    by_suffix("_URL").or_else(|| by_suffix("_ENDPOINT"))
}

/// The session index: each readable record. A file another command removes between listing and
/// reading is skipped.
/// And each checkout (or, when no other record names it, the record) whose session record, the
/// index's or the project copy, cannot be read, with the record's error.
fn indexed_sessions(state: &Path) -> Result<(Vec<Session>, IndexMap<PathBuf, String>)> {
    let Ok(entries) = fs::read_dir(state.join("sessions")) else {
        return Ok((Vec::new(), IndexMap::new()));
    };
    let mut out = Vec::new();
    let mut unreadable = IndexMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // An unreadable record is listed as such; the others are listed as usual.
        let session = match session::readable_session(state, &path)? {
            Ok(Some(session)) => session,
            Ok(None) => continue,
            Err((checkout, e)) => {
                unreadable.insert(checkout, e.message);
                continue;
            }
        };
        if session.project.as_os_str().is_empty() {
            continue;
        }
        out.push(session);
    }
    Ok((out, unreadable))
}

fn accepting(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_ok()
}

/// Bytes a directory tree occupies on disk, without following symbolic links. `None` when the
/// path is missing or not a directory.
fn disk_usage(path: &Path) -> Option<u64> {
    if !fs::symlink_metadata(path).ok()?.is_dir() {
        return None;
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            // A directory entry's metadata does not follow a link.
            let Ok(meta) = entry.metadata() else { continue };
            total = total.saturating_add(on_disk(&meta));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Some(total)
}

#[cfg(unix)]
fn on_disk(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn on_disk(meta: &fs::Metadata) -> u64 {
    meta.len()
}

/// `12.3 MiB`.
pub fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// `ps` for people: one line per checkout, one aligned line per service under it.
pub fn print_ps(report: &PsReport) {
    if report.checkouts.is_empty() {
        println!("no checkouts recorded on this machine");
        return;
    }
    let width = |f: &dyn Fn(&ServiceRow) -> usize| report.checkouts.iter().flat_map(|c| &c.services).map(f).max().unwrap_or(0);
    let name_w = width(&|r| r.name.len());
    let port_w = width(&|r| r.port.map_or(1, |p| p.to_string().len()));
    let pid_w = width(&|r| r.pid.map_or(1, |p| p.to_string().len()));
    let now = now();
    for c in &report.checkouts {
        let mut line = format!("{}  {}", c.path.display(), c.state);
        if c.unreadable.is_some() {
            line.push_str(" (session record cannot be read; run `stack down` in it)");
        }
        if c.dir != "present" {
            line.push_str(&format!(" (directory {})", c.dir));
        }
        if let Some(s) = &c.session {
            line.push_str(&format!("  session {}", s.id));
        }
        if let Some(l) = &c.lease {
            line.push_str(&match (l.kind, l.expires_at) {
                ("none", _) => String::new(),
                _ if l.expired => format!("  lease {} expired", l.kind),
                (kind, Some(at)) => format!("  lease {kind}, {}s left", at.saturating_sub(now)),
                (kind, None) => format!("  lease {kind}"),
            });
        }
        match (&c.branch, &c.head) {
            (Some(branch), Some(head)) => line.push_str(&format!("  branch {branch} @ {head}")),
            (Some(branch), None) => line.push_str(&format!("  branch {branch}")),
            (None, Some(head)) => line.push_str(&format!("  detached @ {head}")),
            (None, None) => {}
        }
        if !c.labels.is_empty() {
            line.push_str(&format!("  labels {}", labels::shown(&c.labels)));
        }
        if c.commands > 0 {
            line.push_str(&format!("  {} command(s) running", c.commands));
        }
        if c.reclaimable {
            line.push_str("  reclaimable by `stack gc`");
        }
        println!("{line}");
        if !c.watch_changed.is_empty() {
            println!("  watch changed: {} (run `stack restart --changed` in it)", c.watch_changed.join(", "));
        }
        for r in &c.services {
            let live = match (r.pid_alive, r.port_accepting) {
                (Some(true), true) => "pid alive, port open",
                (Some(true), false) => "pid alive, port closed",
                (Some(false), true) => "pid gone, port open",
                (Some(false), false) => "pid gone",
                (None, true) => "port open",
                (None, false) => "port closed",
            };
            let port = r.port.map_or("-".into(), |p| p.to_string());
            let pid = r.pid.map_or("-".into(), |p| p.to_string());
            let mut row = format!("  {:<name_w$}  port {port:<port_w$}  pid {pid:<pid_w$}  {live:<22}", r.name);
            if let Some(reserved) = r.reserved_port {
                row.push_str(&format!("  reserved {reserved}"));
            }
            if let Some(endpoint) = &r.endpoint {
                row.push_str(&format!("  {endpoint}"));
            }
            if let Some(dir) = &r.data_dir {
                let size = r.data_bytes.map(human_bytes).unwrap_or_else(|| "-".into());
                row.push_str(&format!("  {size:>9}  {dir}"));
            }
            println!("{}", row.trim_end());
        }
    }
    let t = &report.totals;
    println!(
        "{} checkout(s), {} running; {} service(s), {} live; {} port(s) reserved; {} of data ({} reclaimable)",
        t.checkouts,
        t.running,
        t.services,
        t.services_live,
        t.ports_reserved,
        human_bytes(t.data_bytes),
        human_bytes(t.reclaimable_bytes)
    );
    println!("liveness only; run `stack status` in a checkout to verify its services");
}

// ---- fleet status, logs and down ------------------------------------------------------------

/// Which checkouts a fleet `status`, `logs` or `down` acts on: those of one project, those with
/// labels, or (`all`) every checkout Stack has records for; only ones whose directory is still
/// there and still the one recorded. Several criteria must all hold.
#[derive(Debug, Clone, Default)]
pub struct FleetFilter {
    /// Checkouts of this directory's project (see [`Checkout::project_id`]).
    pub project: Option<PathBuf>,
    /// Checkouts whose session has every one of these labels.
    pub labels: Labels,
    /// Every checkout (narrowed by `labels` when also given).
    pub all: bool,
}

impl FleetFilter {
    /// Whether a fleet selection was asked for at all: otherwise the command acts on one checkout.
    pub fn active(&self) -> bool {
        self.all || self.project.is_some() || !self.labels.is_empty()
    }
}

/// A checkout a fleet command acted on.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: Labels,
    /// Session ID observed during selection, rechecked under the target lock before mutating.
    #[serde(skip)]
    pub session_id: Option<String>,
}

impl Target {
    /// Its branch, else its directory's name: how people tell checkouts of one project apart.
    pub fn name(&self) -> String {
        self.branch.clone().unwrap_or_else(|| self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned()))
    }
}

/// The checkouts `filter` selects, in path order. `manifest`: for commands that compile the
/// project, skip unstarted port-only reservations that lack a `stack.toml`, while keeping
/// started checkouts (those with a session or unconfirmed down) so missing-manifest errors are
/// reported rather than silently hidden.
pub fn select(state: &Path, filter: &FleetFilter, manifest: bool) -> Result<Vec<Target>> {
    let records = Records::read(state)?;
    let scope = filter.project.as_deref().map(|p| crate::snapshot::project_id_offline(p).id);
    let mut out = Vec::new();
    for path in records.paths() {
        if records.dir(&path) != "present" {
            continue;
        }
        let session = records.sessions.get(&path);
        let unconfirmed = if session.is_none() { session::unconfirmed_labels(state, &path) } else { None };
        let has_active_state = session.is_some() || unconfirmed.is_some();
        if manifest && !has_active_state && !path.join(manifest::PROJECT_FILE).is_file() {
            continue;
        }
        let labels = session
            .map(|s| s.labels.clone())
            .or(unconfirmed)
            .unwrap_or_default();
        if !labels::matches(&labels, &filter.labels) {
            continue;
        }
        let git = gitdir::locate(&path);
        if scope.as_ref().is_some_and(|s| *s != crate::snapshot::project_id_of(&path, git.as_ref()).id) {
            continue;
        }
        let branch = git.as_ref().and_then(|g| gitdir::head(g).branch);
        let session_id = session.map(|s| s.id.clone());
        out.push(Target { path, branch, labels, session_id });
    }
    Ok(out)
}

fn ctx_for(state: &Path, cache: &Path, target: &Target) -> Ctx {
    Ctx { root: target.path.clone(), cache: cache.to_path_buf(), state: state.to_path_buf() }
}

/// One checkout's `status` in a fleet status.
#[derive(Debug, Serialize)]
pub struct FleetStatusEntry {
    #[serde(flatten)]
    pub checkout: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusReport>,
    /// Why its status could not be told (its stack.toml no longer compiles, say).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StackError>,
}

/// `stack status --project | --all | --label`.
#[derive(Debug, Serialize)]
pub struct FleetStatusReport {
    pub checkouts: Vec<FleetStatusEntry>,
    /// Checkouts checked.
    pub total: usize,
    /// Checkouts with anything started (`started` in their status).
    pub running: usize,
    /// Started checkouts whose every service verified.
    pub healthy: usize,
    /// Every started checkout is healthy and every status could be told: `stack status` exits
    /// 0 exactly then.
    pub passed: bool,
}

/// `status` of every checkout `filter` selects, one after another (each under its own lock).
pub fn status(state: &Path, cache: &Path, filter: &FleetFilter) -> Result<FleetStatusReport> {
    let mut checkouts = Vec::new();
    for target in select(state, filter, true)? {
        let (status, error) = match session::status_readonly(&ctx_for(state, cache, &target)) {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e)),
        };
        checkouts.push(FleetStatusEntry { checkout: target, status, error });
    }
    let started: Vec<&StatusReport> = checkouts.iter().filter_map(|c| c.status.as_ref()).filter(|s| s.started).collect();
    let healthy = started.iter().filter(|s| s.healthy).count();
    Ok(FleetStatusReport {
        total: checkouts.len(),
        running: started.len(),
        healthy,
        passed: healthy == started.len() && checkouts.iter().all(|c| c.error.is_none()),
        checkouts,
    })
}

/// One checkout's logs in a fleet `logs`.
#[derive(Debug, Serialize)]
pub struct FleetLogsEntry {
    #[serde(flatten)]
    pub checkout: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<crate::logs::Logs>,
    /// Why its logs could not be read (nothing launched there yet, say).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StackError>,
}

/// `stack logs --project | --label`.
#[derive(Debug, Serialize)]
pub struct FleetLogsReport {
    pub checkouts: Vec<FleetLogsEntry>,
}

/// What `q` selects of every checkout `filter` selects. A service name `q` gives that a checkout
/// does not define is that checkout's error, not the whole call's.
pub fn logs(state: &Path, cache: &Path, filter: &FleetFilter, q: &crate::logs::Query) -> Result<FleetLogsReport> {
    let mut checkouts = Vec::new();
    for target in select(state, filter, true)? {
        let (logs, error) = match crate::logs::fetch(&ctx_for(state, cache, &target), q) {
            Ok(l) => (Some(l), None),
            Err(e) => (None, Some(e)),
        };
        checkouts.push(FleetLogsEntry { checkout: target, logs, error });
    }
    Ok(FleetLogsReport { checkouts })
}

/// One checkout's `down` in a fleet down.
#[derive(Debug, Serialize)]
pub struct FleetDownEntry {
    #[serde(flatten)]
    pub checkout: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down: Option<DownReport>,
    /// Why it could not be stopped, or its stop not confirmed (`stop_unconfirmed` and its details).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StackError>,
}

/// `stack down --project | --label`.
#[derive(Debug, Serialize)]
pub struct FleetDownReport {
    pub checkouts: Vec<FleetDownEntry>,
    /// Checkouts whose stop was confirmed.
    pub confirmed: usize,
    /// Checkouts that failed or were not confirmed: `stack down` exits 1 when any did.
    pub failed: usize,
}

/// `down` of every checkout `filter` selects, one after another; one that fails does not keep
/// the others from stopping.
pub fn down(state: &Path, cache: &Path, filter: &FleetFilter, stop_supervisor: bool, purge: bool) -> Result<FleetDownReport> {
    let mut checkouts = Vec::new();
    for target in select(state, filter, false)? {
        let (down, error) = match session::down_matching(
            &ctx_for(state, cache, &target),
            stop_supervisor,
            purge,
            &filter.labels,
            target.session_id.as_deref(),
        ) {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e)),
        };
        checkouts.push(FleetDownEntry { checkout: target, down, error });
    }
    let confirmed = checkouts.iter().filter(|c| c.down.as_ref().is_some_and(|d| d.confirmed)).count();
    Ok(FleetDownReport { failed: checkouts.len() - confirmed, confirmed, checkouts })
}

/// A fleet status for people: a line per checkout, and one per service that did not verify.
pub fn print_fleet_status(r: &FleetStatusReport) {
    if r.checkouts.is_empty() {
        println!("no checkout matches");
        return;
    }
    for c in &r.checkouts {
        let what = match (&c.status, &c.error) {
            (_, Some(e)) => format!("error[{}]: {}", e.code, e.message),
            (Some(s), _) if !s.started => "not started".to_string(),
            (Some(s), _) if s.healthy => format!("healthy{}", s.session.as_ref().map(|s| format!("  session {}", s.id)).unwrap_or_default()),
            (Some(s), _) => format!("NOT HEALTHY{}", if s.stale { " (stale: compiled configuration changed)" } else { "" }),
            (None, None) => unreachable!("a status or an error"),
        };
        println!("{}  [{}]  {what}", c.checkout.path.display(), c.checkout.name());
        for check in c.status.iter().filter(|s| s.started).flat_map(|s| &s.checks) {
            if let Some(reason) = &check.reason {
                println!("  {:<12} NOT READY  {reason}", check.service);
            }
        }
    }
    println!("{} checkout(s): {} started, {} healthy", r.total, r.running, r.healthy);
}

/// Fleet logs for people: each line prefixed `[<branch or directory>:<service>]`; what could
/// not be read on stderr. Fails when stdout does.
pub fn print_fleet_logs(r: &FleetLogsReport) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for c in &r.checkouts {
        let name = c.checkout.name();
        if let Some(e) = &c.error {
            eprintln!("# {} [{name}]: error[{}]: {}", c.checkout.path.display(), e.code, e.message);
        }
        let services: Vec<(&str, &[String])> = match &c.logs {
            Some(crate::logs::Logs::One(l)) => vec![(l.service.as_str(), l.lines.as_slice())],
            Some(crate::logs::Logs::Many(m)) => {
                for (service, e) in &m.errors {
                    eprintln!("# {} [{name}:{service}]: error[{}]: {}", c.checkout.path.display(), e.code, e.message);
                }
                m.services.iter().map(|(s, l)| (s.as_str(), l.lines.as_slice())).collect()
            }
            None => Vec::new(),
        };
        for (service, lines) in services {
            for line in lines {
                writeln!(out, "[{name}:{service}] {line}")?;
            }
        }
    }
    out.flush()
}

/// A fleet down for people: a line per checkout and a total.
pub fn print_fleet_down(r: &FleetDownReport) {
    if r.checkouts.is_empty() {
        println!("no checkout matches");
        return;
    }
    for c in &r.checkouts {
        let what = match (&c.down, &c.error) {
            (_, Some(e)) => format!("error[{}]: {}", e.code, e.message),
            (Some(d), _) => format!("stopped {} service(s); {}", d.stopped.len(), if d.confirmed { "confirmed" } else { "NOT confirmed" }),
            (None, None) => unreachable!("a report or an error"),
        };
        println!("{}  [{}]  {what}", c.checkout.path.display(), c.checkout.name());
        if let Some(purge) = c.down.as_ref().and_then(|d| d.purge.as_ref()) {
            for d in &purge.data {
                println!("  {} {}: {}", if d.kept { "kept" } else { "data" }, d.service, d.outcome);
            }
        }
    }
    println!("{} checkout(s) stopped and confirmed, {} failed", r.confirmed, r.failed);
}

// ---- mv ------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MvReport {
    pub from: PathBuf,
    pub to: PathBuf,
    /// The session id moved, if one was recorded.
    pub session: Option<String>,
    pub ports: usize,
    pub identity_tokens: usize,
    /// Service data directories moved to where the provider looks for them at the new path.
    pub data: Vec<MovedData>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MovedData {
    pub service: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A recorded data directory of the moved checkout, checked and held open, and where it goes.
struct DataMove {
    entry: datadirs::Entry,
    held: datadirs::Held,
    to: PathBuf,
}

/// `stack mv <old> <new>`: the checkout at `old` was moved to `new`; move what Stack recorded
/// for it (session, port reservations, identity tokens, completion records) to the new path,
/// and the service data directories `up` recorded for it to where the provider keeps data for
/// the new path. Relative paths are resolved against `base`.
///
/// The checkout token is derived from the path (see [`checkout`]), so it changes with the move:
/// each data directory moved gets the new token in its marker and its record, as `up` would
/// write them, and data recorded for another directory once at `old` stays where it is, for
/// `gc` to report. Only with nothing running: services were started with the old path in their
/// supervisor configuration and carry the old token, so they can only be stopped from there.
/// Everything is checked before anything changes; a failure part way is undone where it can be.
pub fn mv(state: &Path, base: &Path, old: &Path, new: &Path) -> Result<MvReport> {
    let new = normalize(&base.join(new)).canonicalize().ok().filter(|p| p.is_dir()).ok_or_else(|| {
        StackError::new("mv_target_missing", format!("{} is not an existing directory", new.display())).hint(format!(
            "move the checkout to {} first (with `git worktree move` or `mv`), then run `stack mv {} {}`",
            new.display(),
            old.display(),
            new.display()
        ))
    })?;
    let old = resolve_old(state, &normalize(&base.join(old)), &new)?;
    if old == new {
        return Err(StackError::new("usage", format!("{} and {} are the same path", old.display(), new.display())));
    }
    if old.exists() {
        return Err(StackError::new("mv_source_exists", format!("{} still exists, so it was not moved", old.display())).hint(format!(
            "`stack mv` follows a checkout that was already moved: move {} to {} first, then run `stack mv {} {}`; \
             a copy needs no `stack mv` (it gets its own ports on `stack up`)",
            old.display(),
            new.display(),
            old.display(),
            new.display()
        )));
    }

    // Both projects' lifecycles are held, in a fixed order, so no `up` (or `gc`) runs on either.
    let mut keys = [old.clone(), new.clone()];
    keys.sort_by_key(|p| project_key(p));
    let _locks: Vec<FileLock> = keys.iter().map(|p| project_lock(state, p)).collect::<Result<_>>()?;

    // A session at the new path is another checkout's running or recorded instance. Ports and
    // tokens alone are only what a compile there assigned (a worktree hook compiles before
    // anything else runs), so the old checkout's replace them.
    if session::index_path(state, &new).exists() {
        return Err(StackError::new("mv_target_has_state", format!("{} already has a session of its own", new.display())).hint(format!(
            "run `stack -C {} down` to end it, or move the checkout to another path",
            new.display()
        )));
    }
    let old_index = session::index_path(state, &old);
    let mirror = new.join(".stack").join("session.json");
    // Either record unreadable refuses the move (see `session::session_record`).
    let mut session: Option<Session> = match session::session_record(&old_index)? {
        Some(indexed) => Some(indexed),
        None => session::session_record(&mirror)?.filter(|s| s.project == old),
    };

    // What `up` recorded for this very directory carries the token it had at `old`.
    let moved_token = checkout::token_as(&old, &new);
    let new_token = checkout::token(&new);
    let (own, earlier): (Vec<datadirs::Entry>, Vec<datadirs::Entry>) = datadirs::entries(state)?
        .into_iter()
        .filter(|e| e.project == old)
        .partition(|e| moved_token.as_deref() == Some(e.checkout.as_str()));

    let running = still_running(session.as_ref(), &own, moved_token.as_deref());
    if !running.is_empty() {
        return Err(StackError::new("mv_services_running", format!("services recorded for {} may still be running", old.display()))
            .hint(format!(
                "they were started with the old path in their configuration and can only be stopped from there: move the \
                 directory back to {old}, run `stack -C {old} down`, move it to {new} again, then run `stack mv {old} {new}`",
                old = old.display(),
                new = new.display()
            ))
            .details(running.into_iter().map(serde_json::Value::String).collect()));
    }

    let mut notes = Vec::new();
    if session.as_ref().is_some_and(|s| s.project_dir_id.is_some() && s.project_dir_id != session::dir_id(&new)) {
        notes.push(format!(
            "{} is not the directory recorded at {} (copied rather than moved?); its session was moved anyway, since nothing runs",
            new.display(),
            old.display()
        ));
    }
    if !earlier.is_empty() {
        notes.push(format!(
            "data recorded for an earlier directory at {} stays where it is; `stack gc` reports it and `stack gc --reclaim-data` removes it",
            old.display()
        ));
    }
    let (moves, settled) = plan_data_moves(state, session.as_ref(), own, &earlier, &old, &new, new_token.as_deref(), &mut notes)?;

    // Changes start here. Each step undoes the earlier ones if it fails.
    let (ports_moved, ports_replaced) = ports::rekey(state, &old, &new)?;
    // Nothing runs from the old path (checked above): its supervisor's records and logs go.
    crate::supervisor::forget(&crate::supervisor::dir(state, &old));
    let (tokens_moved, tokens_replaced) = identity::rekey(state, &old, &new).inspect_err(|_| {
        let _ = ports::rekey(state, &new, &old);
    })?;
    let undo_registries = || {
        let _ = ports::rekey(state, &new, &old);
        let _ = identity::rekey(state, &new, &old);
    };
    let mut done: Vec<(datadirs::Entry, PathBuf)> = Vec::new();
    for m in moves {
        let moved = move_data(m, new_token.as_deref().unwrap_or_default());
        match moved {
            Ok(pair) => done.push(pair),
            Err(e) => {
                undo_moves(&done);
                undo_registries();
                return Err(e);
            }
        }
    }
    let recorded = (|| {
        let moved: Vec<&datadirs::Entry> = done.iter().map(|(e, _)| e).collect();
        datadirs::forget(state, |e| moved.contains(&e) || settled.contains(e))?;
        for (entry, to) in &done {
            let (location, identity) = datadirs::locate(to);
            datadirs::record(
                state,
                datadirs::Entry {
                    project: new.clone(),
                    service: entry.service.clone(),
                    dir: to.clone(),
                    checkout: new_token.clone().unwrap_or_default(),
                    supervisor: entry.supervisor.clone(),
                    location,
                    identity,
                    project_at: crate::checkout::Recorded::of(&new),
                },
            )?;
        }
        Ok::<(), StackError>(())
    })();
    if let Err(e) = recorded {
        undo_moves(&done);
        undo_registries();
        return Err(e);
    }
    if ports_replaced + tokens_replaced > 0 {
        notes.push(format!(
            "replaced {ports_replaced} port reservation(s) and {tokens_replaced} identity token(s) a compile at {} had assigned",
            new.display()
        ));
    }
    if ports_moved == 0 && session.as_ref().is_some_and(|s| !s.services.is_empty()) {
        notes.push(format!(
            "no port reservation was left for {} (a compile elsewhere released it once the path was gone); the next `stack up` assigns ports",
            old.display()
        ));
    }
    if let Some(session) = session.as_mut() {
        session.project = new.clone();
        session.project_dir_id = session::dir_id(&new);
        session.project_dir_created = session::dir_created(&new);
        session.project_location = crate::checkout::location(&new);
        for (entry, to) in &done {
            if let Some(record) = session.services.get_mut(&entry.service) {
                record.data_dir = Some(to.to_string_lossy().into_owned());
            }
        }
        session::save_at(state, &new, session)?;
        match fs::remove_file(&old_index) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(old_index.display(), e)),
        }
    }
    // Completion records name the session and execution, not the path; they follow the key.
    let (old_completed, new_completed) = (session::completions_dir(state, &old), session::completions_dir(state, &new));
    if old_completed.exists() && !new_completed.exists() {
        let _ = fs::rename(&old_completed, &new_completed);
    }
    // Init markers follow the checkout, and name moved data directories where they are now.
    let moved: Vec<(PathBuf, PathBuf)> =
        done.iter().flat_map(|(e, to)| e.location.iter().chain([&e.dir]).map(move |from| (from.clone(), to.clone()))).collect();
    crate::init_tasks::rekey(state, &old, &new, &moved);
    // Data that came from where mise's presets kept it for an older Stack leaves mise's own
    // small record of the old path behind; nothing of Stack's stays at the old path.
    if let Some((from, _)) = done.iter().find(|(e, _)| e.dir.components().any(|c| c.as_os_str() == "daemons")) {
        let record = from.dir.parent().and_then(Path::parent).map(|p| p.display().to_string()).unwrap_or_default();
        notes.push(format!("{} came from the directory mise's preset kept for the old path; mise's own record of that path ({record}) stays, and can be deleted", from.dir.display()));
    }
    Ok(MvReport {
        from: old,
        to: new,
        session: session.map(|s| s.id),
        ports: ports_moved,
        identity_tokens: tokens_moved,
        data: done.into_iter().map(|(e, to)| MovedData { service: e.service, from: e.dir, to }).collect(),
        notes,
    })
}

/// The recorded path `old` names. A path that no longer exists cannot be canonicalized, so the
/// lexical path and the one through its parent's canonical form (`/tmp` and `/private/tmp`)
/// are both tried, and the first with anything recorded wins.
fn resolve_old(state: &Path, old: &Path, new: &Path) -> Result<PathBuf> {
    let mut candidates = vec![old.to_path_buf()];
    if let (Some(parent), Some(name)) = (old.parent(), old.file_name()) {
        if let Ok(parent) = parent.canonicalize() {
            candidates.push(parent.join(name));
        }
    }
    if let Ok(canonical) = old.canonicalize() {
        candidates.push(canonical);
    }
    let reservations = ports::all(state)?;
    let tokens = identity::holders(state)?;
    let data = datadirs::entries(state)?;
    let mirror: Option<Session> = session::session_record(&new.join(".stack").join("session.json"))?;
    let recorded = |c: &PathBuf| {
        session::index_path(state, c).exists()
            || reservations.iter().any(|r| r.project == *c)
            || tokens.iter().any(|(p, _)| p == c)
            || data.iter().any(|e| e.project == *c)
            || mirror.as_ref().is_some_and(|m| m.project == *c)
    };
    candidates.iter().find(|c| recorded(c)).cloned().ok_or_else(|| {
        StackError::new("mv_nothing_recorded", format!("nothing is recorded for {}", old.display()))
            .hint("run `stack ps` to list the checkouts Stack has records for, and pass one of those paths")
    })
}

/// Reasons to think something of the moved checkout still runs: what its session records (a
/// live pid, an open port, a supervisor that does not report the service stopped, a launch in
/// progress), a supervisor its data record names that does not report the service stopped, or
/// a process carrying its checkout token. Empty means nothing does, as far as can be told.
fn still_running(session: Option<&Session>, own: &[datadirs::Entry], token: Option<&str>) -> Vec<String> {
    let mut reasons = Vec::new();
    let stopped = |supervisor: &datadirs::Supervisor| session::service_stopped(supervisor).err();
    if let Some(session) = session {
        if session.launching {
            reasons.push("a startup was interrupted or is still in progress".to_string());
        }
        for (name, record) in &session.services {
            if let Some(pid) = record.pid.filter(|p| pid_alive(*p)) {
                reasons.push(format!("{name}: pid {pid} is alive"));
            }
            if record.port != 0 && accepting(record.port) {
                reasons.push(format!("{name}: port {} accepts connections (possibly another program)", record.port));
            }
            if let Some(provider) = &session.provider {
                let id = record.provider_id.clone().unwrap_or_else(|| name.clone());
                let supervisor = datadirs::Supervisor { pitchfork: provider.pitchfork.clone(), state_dir: provider.state_dir.clone(), id };
                reasons.extend(stopped(&supervisor).map(|why| format!("{name}: {why}")));
            }
        }
    }
    for entry in own {
        if let Some(s) = &entry.supervisor {
            reasons.extend(stopped(s).map(|why| format!("{}: {why}", entry.service)));
        }
    }
    let mut tokens: Vec<&str> = token.into_iter().chain(own.iter().map(|e| e.checkout.as_str())).collect();
    tokens.sort_unstable();
    tokens.dedup();
    for token in tokens {
        for process in crate::procinfo::carrying(checkout::TOKEN_VAR, token) {
            reasons.push(format!("process {} carries the checkout's token", process.pid));
        }
    }
    reasons
}

fn unmovable(service: &str, why: String) -> StackError {
    StackError::new("mv_data_unmovable", format!("cannot move {service}'s data: {why}"))
}

/// Check each data directory `up` recorded for the moved checkout as `gc --reclaim-data` checks
/// it (see [`datadirs::hold`]: where it resolved, no link, the same directory, its marker), and
/// find where the provider will look for it at `new`. Returns what to move, and the records to
/// drop: data already gone, or no longer carrying the checkout's marker (as `gc` settles them).
/// Data that cannot be told to be the checkout's own, or placed, refuses the whole move.
#[allow(clippy::too_many_arguments)]
fn plan_data_moves(
    state: &Path,
    session: Option<&Session>,
    own: Vec<datadirs::Entry>,
    earlier: &[datadirs::Entry],
    old: &Path,
    new: &Path,
    new_token: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<(Vec<DataMove>, Vec<datadirs::Entry>)> {
    // Data a session names that the data record does not (recorded before it existed) cannot be
    // told to be this checkout's, so it is never moved and the move is refused.
    if let Some(session) = session {
        for (name, record) in &session.services {
            let Some(dir) = record.data_dir.as_deref() else { continue };
            let in_record = own.iter().chain(earlier).any(|e| e.dir == Path::new(dir));
            if !in_record && fs::symlink_metadata(dir).is_ok() {
                return Err(unmovable(name, format!("{dir} is not in Stack's data record, so Stack cannot tell it is this checkout's")).hint(format!(
                    "move the directory back to {old}, run `stack -C {old} up` and `stack -C {old} down` so it is recorded, move it again, \
                     then retry `stack mv`; or move the data yourself",
                    old = old.display()
                )));
            }
        }
    }
    let mut held = Vec::new();
    let mut settled = Vec::new();
    for entry in own {
        let shown = entry.dir.display().to_string();
        match datadirs::hold(&entry) {
            Found::Gone => {
                notes.push(format!("{}'s data directory {shown} no longer exists; its record is dropped", entry.service));
                settled.push(entry);
            }
            Found::Unbound(_) if matches!(fs::symlink_metadata(&entry.dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound) => {
                notes.push(format!("{}'s data directory {shown} was never created; its record is dropped", entry.service));
                settled.push(entry);
            }
            Found::Unbound(why) | Found::Other(why) => return Err(unmovable(&entry.service, format!("{shown}: {why}"))),
            Found::Held(h) => {
                if h.beside(session::MARKER_SUFFIX).as_deref().and_then(session::marker_owner).as_deref() != Some(entry.checkout.as_str()) {
                    notes.push(format!("{}'s data in {shown} no longer carries this checkout's marker; left where it is", entry.service));
                    settled.push(entry);
                    continue;
                }
                held.push((entry, h));
            }
        }
    }
    if held.is_empty() {
        return Ok((Vec::new(), settled));
    }
    let Some(_) = new_token else {
        return Err(StackError::new("mv_data_unmovable", format!("cannot tell which directory {} is, so its data cannot be marked as its own", new.display())));
    };
    let mut moves = Vec::new();
    for (entry, h) in held {
        // Where Stack keeps the data of a checkout at the new path.
        let to = crate::supervisor::new_data_dir(state, new, &entry.service);
        check_move_target(&to).map_err(|why| unmovable(&entry.service, format!("{} {why}", to.display())))?;
        moves.push(DataMove { entry, held: h, to });
    }
    Ok((moves, settled))
}

/// A data directory may be moved only to an absolute, normalized path where nothing is yet (an
/// empty directory counts as nothing) and no checkout marker lies beside it.
fn check_move_target(to: &Path) -> std::result::Result<(), String> {
    if !to.is_absolute() || to.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("is not an absolute, normalized path".into());
    }
    if fs::symlink_metadata(session::data_marker(to)).is_ok() {
        return Err("has another checkout's marker beside it".into());
    }
    match fs::symlink_metadata(to) {
        Ok(meta) if meta.file_type().is_symlink() => Err("is a symbolic link".into()),
        Ok(meta) if !meta.is_dir() => Err("exists and is not a directory".into()),
        Ok(_) if fs::read_dir(to).map_err(|e| e.to_string())?.next().is_some() => Err("already holds data".into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Move one held data directory to its place and mark it as the checkout's under its new token.
fn move_data(m: DataMove, token: &str) -> Result<(datadirs::Entry, PathBuf)> {
    let DataMove { entry, held, to } = m;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    }
    held.move_to(&to, session::MARKER_SUFFIX)
        .map_err(|e| unmovable(&entry.service, format!("moving {} to {}: {e}", entry.dir.display(), to.display())))?;
    if let Err(e) = write_json(&session::data_marker(&to), &json!({ "checkout": token })) {
        undo_moves(&[(entry, to)]);
        return Err(e);
    }
    Ok((entry, to))
}

/// Put moved data directories back, with the markers they had.
fn undo_moves(done: &[(datadirs::Entry, PathBuf)]) {
    for (entry, to) in done.iter().rev() {
        let _ = fs::remove_file(session::data_marker(to));
        if fs::rename(to, &entry.dir).is_ok() {
            let _ = write_json(&session::data_marker(&entry.dir), &json!({ "checkout": entry.checkout }));
        }
    }
}

pub fn print_mv(r: &MvReport) {
    println!("moved {} -> {}", r.from.display(), r.to.display());
    if let Some(id) = &r.session {
        println!("session {id}");
    }
    println!("{} port reservation(s), {} identity token(s)", r.ports, r.identity_tokens);
    for d in &r.data {
        println!("data {}: {} -> {}", d.service, d.from.display(), d.to.display());
    }
    for n in &r.notes {
        println!("note: {n}");
    }
}

/// `a/./b/../c` as `a/c`, without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn disk_usage_counts_files_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("big"), vec![1u8; 64 * 1024]).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, vec![1u8; 1024 * 1024]).unwrap();
        std::os::unix::fs::symlink(&outside, data.join("link")).unwrap();
        let used = disk_usage(&data).unwrap();
        assert!((64 * 1024..1024 * 1024).contains(&used), "{used}");
        assert_eq!(disk_usage(&dir.path().join("missing")), None);
        assert_eq!(disk_usage(&outside), None, "not a directory");
    }

    #[test]
    fn a_move_target_must_be_free() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(check_move_target(&root.join("new/data/db")), Ok(()));
        fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(check_move_target(&root.join("empty")), Ok(()));
        fs::write(root.join("empty/rows"), "x").unwrap();
        assert!(check_move_target(&root.join("empty")).unwrap_err().contains("already holds data"));
        fs::write(root.join("file"), "x").unwrap();
        assert!(check_move_target(&root.join("file")).unwrap_err().contains("not a directory"));
        std::os::unix::fs::symlink(root.join("new"), root.join("link")).unwrap();
        assert!(check_move_target(&root.join("link")).unwrap_err().contains("symbolic link"));
        fs::write(root.join("marked.stack-checkout"), "{}").unwrap();
        assert!(check_move_target(&root.join("marked")).unwrap_err().contains("marker"));
        assert!(check_move_target(Path::new("relative/db")).unwrap_err().contains("absolute"));
        assert!(check_move_target(&root.join("a/../b")).unwrap_err().contains("normalized"));
    }

    #[test]
    fn paths_normalize_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn sizes_read_for_people() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    fn git(at: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A session record of `root` with `services` (name, port, pid, started_at_ms) and `labels`.
    fn record(state: &Path, root: &Path, services: &[(&str, u16, Option<u32>, Option<u64>)], labels: &[(&str, &str)]) {
        let session = Session {
            id: format!("s-{}", root.file_name().unwrap().to_string_lossy()),
            project: root.to_path_buf(),
            started_at: 1,
            services: services
                .iter()
                .map(|(name, port, pid, started)| {
                    (name.to_string(), session::ServiceRecord { port: *port, pid: *pid, data_dir: None, identity: session::Identity::Liveness, verified_at: 1, started_at_ms: *started, provider_id: None })
                })
                .collect(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Session::default()
        };
        session::save_at(state, root, &session).unwrap();
    }

    /// A repository `repo` (stack.toml committed) with a worktree `wt` on branch `feature`, and a
    /// directory `lone` outside Git; all canonical.
    fn checkouts(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let top = dir.canonicalize().unwrap();
        let repo = top.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("stack.toml"), "[env]\nA = 'b'\n").unwrap();
        fs::write(repo.join(".gitignore"), ".stack/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "x"]);
        let wt = top.join("wt");
        git(&repo, &["worktree", "add", "-q", "-b", "feature", wt.to_str().unwrap()]);
        let lone = top.join("lone");
        fs::create_dir(&lone).unwrap();
        fs::write(lone.join("stack.toml"), "").unwrap();
        (repo, wt, lone)
    }

    fn paths(report: &PsReport) -> Vec<&Path> {
        report.checkouts.iter().map(|c| c.path.as_path()).collect()
    }

    #[test]
    fn ps_selects_by_project_state_and_labels_and_says_where_each_checkout_is() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let (repo, wt, lone) = checkouts(dir.path());
        let sha = git(&repo, &["rev-parse", "HEAD"]);
        record(&state, &repo, &[], &[("agent", "claude"), ("task", "a")]);
        record(&state, &wt, &[("web", 1, None, None)], &[("agent", "codex")]);
        record(&state, &lone, &[], &[("agent", "claude")]);

        // Without options: everything, as before, and each checkout says where it is.
        let all = ps(&state, false).unwrap();
        assert_eq!(paths(&all), vec![lone.as_path(), repo.as_path(), wt.as_path()]);
        let row = |r: &PsReport, p: &Path| r.checkouts.iter().find(|c| c.path == p).unwrap().clone_view();
        let (r, w, l) = (row(&all, &repo), row(&all, &wt), row(&all, &lone));
        assert_eq!((r.0.as_deref(), r.1.as_deref()), (Some("main"), Some(&sha[..7])));
        assert_eq!((w.0.as_deref(), w.1.as_deref()), (Some("feature"), Some(&sha[..7])));
        assert_eq!((l.0, l.1), (None, None), "outside Git");
        assert_eq!(r.2, w.2, "one project for a repository's worktrees");
        assert_ne!(r.2, l.2);
        assert_eq!(r.2.as_deref(), Some(crate::snapshot::project_id(&repo).id.as_str()), "the identity snapshots share");
        assert_eq!(all.totals.checkouts, 3);

        let with = |opts: PsOptions| ps_with(&state, false, &opts).unwrap();
        let labels = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Labels>();
        // By project: the repository's worktrees, from either of them.
        for from in [&repo, &wt] {
            assert_eq!(paths(&with(PsOptions { project: Some(from.clone()), ..Default::default() })), vec![repo.as_path(), wt.as_path()]);
        }
        assert_eq!(paths(&with(PsOptions { project: Some(lone.clone()), ..Default::default() })), vec![lone.as_path()]);
        // By state: a session without services is `recorded`; one whose service is gone `stopped`.
        assert_eq!(paths(&with(PsOptions { state: Some("stopped".into()), ..Default::default() })), vec![wt.as_path()]);
        assert_eq!(paths(&with(PsOptions { state: Some("recorded".into()), ..Default::default() })), vec![lone.as_path(), repo.as_path()]);
        assert!(with(PsOptions { state: Some("running".into()), ..Default::default() }).checkouts.is_empty());
        let e = ps_with(&state, false, &PsOptions { state: Some("happy".into()), ..Default::default() }).unwrap_err();
        assert_eq!(e.code, "usage");
        assert!(e.hint.unwrap().contains("unstarted"));
        // By labels: every pair must match; combined with a project, both must hold.
        assert_eq!(paths(&with(PsOptions { labels: labels(&[("agent", "claude")]), ..Default::default() })), vec![lone.as_path(), repo.as_path()]);
        assert_eq!(paths(&with(PsOptions { labels: labels(&[("agent", "claude"), ("task", "a")]), ..Default::default() })), vec![repo.as_path()]);
        assert!(with(PsOptions { labels: labels(&[("agent", "nobody")]), ..Default::default() }).checkouts.is_empty());
        let both = with(PsOptions { project: Some(repo.clone()), labels: labels(&[("agent", "claude")]), ..Default::default() });
        assert_eq!(paths(&both), vec![repo.as_path()]);
        assert_eq!(both.checkouts[0].labels, labels(&[("agent", "claude"), ("task", "a")]));
        // Totals count what is listed.
        assert_eq!(both.totals.checkouts, 1);

        // The fleet commands select the same way, only directories still there.
        let select = |filter: FleetFilter| select(&state, &filter, false).unwrap().into_iter().map(|t| t.path).collect::<Vec<_>>();
        assert_eq!(select(FleetFilter { project: Some(wt.clone()), ..Default::default() }), vec![repo.clone(), wt.clone()]);
        assert_eq!(select(FleetFilter { labels: labels(&[("agent", "codex")]), ..Default::default() }), vec![wt.clone()]);
        assert_eq!(select(FleetFilter { all: true, ..Default::default() }).len(), 3);
        let targets = super::select(&state, &FleetFilter { project: Some(repo.clone()), ..Default::default() }, false).unwrap();
        assert_eq!(targets.iter().map(Target::name).collect::<Vec<_>>(), vec!["main", "feature"]);
        assert_eq!(Target { path: lone.clone(), branch: None, labels: Labels::new(), session_id: None }.name(), "lone");
        assert!(!FleetFilter::default().active());
        fs::remove_dir_all(&lone).unwrap();
        assert_eq!(select(FleetFilter { all: true, ..Default::default() }), vec![repo.clone(), wt.clone()], "a deleted checkout is not acted on");
    }

    impl Checkout {
        fn clone_view(&self) -> (Option<String>, Option<String>, Option<String>) {
            (self.branch.clone(), self.head.clone(), self.project_id.clone())
        }
    }

    #[test]
    fn ps_worktrees_adds_the_projects_worktrees_nothing_is_recorded_for() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let (repo, wt, _lone) = checkouts(dir.path());
        // A third worktree with no stack.toml at all is not a checkout of the project.
        let bare = dir.path().canonicalize().unwrap().join("nostack");
        git(&repo, &["worktree", "add", "-q", "--detach", bare.to_str().unwrap()]);
        fs::remove_file(bare.join("stack.toml")).unwrap();
        record(&state, &repo, &[], &[]);

        let without = ps_with(&state, false, &PsOptions { cwd: Some(repo.clone()), ..Default::default() }).unwrap();
        assert_eq!(paths(&without), vec![repo.as_path()], "only with --worktrees");
        let report = ps_with(&state, false, &PsOptions { worktrees: true, cwd: Some(repo.clone()), ..Default::default() }).unwrap();
        assert_eq!(paths(&report), vec![repo.as_path(), wt.as_path()]);
        let w = &report.checkouts[1];
        assert_eq!((w.state, w.dir, w.dir_exists, w.branch.as_deref()), ("unstarted", "present", true, Some("feature")));
        assert!(w.session.is_none() && w.services.is_empty() && !w.reclaimable);
        assert_eq!(w.project_id, report.checkouts[0].project_id);
        // Through --project from anywhere, and narrowed by state.
        let by_project = ps_with(&state, false, &PsOptions { worktrees: true, project: Some(wt.clone()), state: Some("unstarted".into()), ..Default::default() }).unwrap();
        assert_eq!(paths(&by_project), vec![wt.as_path()]);
        let recorded = ps_with(&state, false, &PsOptions { worktrees: true, cwd: Some(repo.clone()), state: Some("recorded".into()), ..Default::default() }).unwrap();
        assert_eq!(paths(&recorded), vec![repo.as_path()]);
        // A label selection lists only sessions, which an unstarted worktree has none of.
        let labelled = PsOptions { worktrees: true, cwd: Some(repo.clone()), labels: Labels::from([("a".into(), "b".into())]), ..Default::default() };
        assert!(ps_with(&state, false, &labelled).unwrap().checkouts.is_empty());
        // Once recorded, it is listed as what it is.
        record(&state, &wt, &[], &[]);
        let report = ps_with(&state, false, &PsOptions { worktrees: true, cwd: Some(repo.clone()), ..Default::default() }).unwrap();
        assert_eq!(report.checkouts.iter().map(|c| c.state).collect::<Vec<_>>(), vec!["recorded", "recorded"]);
    }

    #[test]
    fn ps_counts_running_commands_and_names_live_endpoints_and_changed_watch_files() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let root = dir.path().canonicalize().unwrap().join("app");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("stack.toml"), "[services.web]\nrun = 'serve'\nwatch = ['src']\n[services.db]\npreset = 'postgres'\n[services.cache]\npreset = 'redis'\n").unwrap();
        fs::write(root.join("src/app.py"), "x").unwrap();
        // Live services: this process's pid; ports something accepts on.
        let listeners: Vec<std::net::TcpListener> = (0..3).map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap()).collect();
        let port = |i: usize| listeners[i].local_addr().unwrap().port();
        let me = Some(std::process::id());
        record(&state, &root, &[("web", port(0), me, Some(1)), ("db", port(1), me, Some(u64::MAX)), ("cache", port(2), me, None)], &[]);

        let report = ps(&state, false).unwrap();
        let c = &report.checkouts[0];
        assert_eq!(c.state, "running");
        let endpoint = |name: &str| c.services.iter().find(|r| r.name == name).unwrap().endpoint.clone();
        assert_eq!(endpoint("web"), Some(format!("http://127.0.0.1:{}", port(0))));
        assert_eq!(endpoint("db"), Some(format!("postgresql://postgres@127.0.0.1:{}/postgres", port(1))));
        assert_eq!(endpoint("cache"), Some(format!("redis://127.0.0.1:{}", port(2))));
        // `web` started long before src/app.py changed; `db` watches nothing.
        assert_eq!(c.watch_changed, vec!["web".to_string()]);
        assert_eq!(c.commands, 0);
        let json = serde_json::to_value(c).unwrap();
        assert!(json.get("commands").is_none() && json.get("labels").is_none(), "empty enrichments are left out: {json}");

        // A command Stack registered for the checkout, while its process (this one) runs.
        let registration = crate::commands::register(&state, &root, crate::commands::StartedBy::Exec, Path::new("/bin/sleep")).unwrap();
        assert_eq!(ps(&state, false).unwrap().checkouts[0].commands, 1);
        drop(registration);

        // A service that is not live has no endpoint.
        drop(listeners);
        record(&state, &root, &[("web", 1, None, Some(1))], &[]);
        let c = &ps(&state, false).unwrap().checkouts[0];
        assert_eq!((c.state, c.services[0].endpoint.as_deref()), ("stopped", None));
        assert!(c.watch_changed.is_empty(), "only live services are checked");
    }

    #[test]
    fn endpoints_follow_the_preset_from_stack_toml_or_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let manifest: ProjectManifest = toml::from_str("[services.q]\npreset = 'nats'\n[override.services.db]\npreset = 'cockroachdb'\n[services.db]\nrun = 'x'\n").unwrap();
        assert_eq!(endpoint(Some(&manifest), "q", None, 4222).as_deref(), Some("nats://127.0.0.1:4222"));
        assert_eq!(endpoint(Some(&manifest), "db", None, 26257).as_deref(), Some("postgresql://root@127.0.0.1:26257/defaultdb?sslmode=disable"), "the override wins");
        // A service a bundle defines: its data directory says which preset it is.
        fs::write(dir.path().join(crate::supervisor::VERSION_FILE), "redis:7").unwrap();
        assert_eq!(endpoint(Some(&manifest), "cache", dir.path().to_str(), 6379).as_deref(), Some("redis://127.0.0.1:6379"));
        assert_eq!(endpoint(None, "web", None, 8080).as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(preset_endpoint("spicedb", 50051).as_deref(), Some("127.0.0.1:50051"));
        assert_eq!(preset_endpoint("nope", 1), None);
    }
}
