//! Machine-wide views and maintenance across checkouts: `ps` lists what Stack knows about, and
//! `mv` follows a checkout that was moved.
//!
//! Everything here reads the same machine state the lifecycle writes (`sessions/`, `ports.json`,
//! `identities.json`, `data.json`). `ps` only reads it, without any lock: every state file is
//! replaced atomically, so a read sees one version or the other, and `ps` never waits behind
//! `up`.

use crate::checkout;
use crate::datadirs::{self, Found};
use crate::error::{io_error, Result, StackError};
use crate::identity;
use crate::lease::LeaseSummary;
use crate::ports;
use crate::provider::mise;
use crate::session::{self, Session};
use crate::state::{now, pid_alive, project_key, project_lock, read_json, write_json, FileLock};
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
    /// `present`, `missing`, or `replaced` (another directory now has this path).
    pub dir: &'static str,
    /// `running` (every service live), `partial`, `stopped`, `launching`, `recorded` (a session
    /// without services), or `reserved` (ports, tokens or data recorded, no session).
    pub state: &'static str,
    pub session: Option<SessionSummary>,
    pub lease: Option<LeaseSummary>,
    pub services: Vec<ServiceRow>,
    pub identity_tokens: usize,
    /// What `stack gc` reclaims for this checkout: its directory is gone and no session of it
    /// is left, so gc releases its ports and reports its recorded data, which `gc
    /// --reclaim-data` removes where it can confirm it is safe to.
    pub reclaimable: bool,
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

/// Every checkout with a session record, port reservations, identity tokens or recorded data.
pub fn ps(state: &Path, sizes: bool) -> Result<PsReport> {
    let mut sessions: BTreeMap<PathBuf, Session> = BTreeMap::new();
    for session in indexed_sessions(state)? {
        sessions.insert(session.project.clone(), session);
    }
    let reservations = ports::all(state)?;
    let tokens = identity::holders(state)?;
    let data = datadirs::entries(state)?;
    let mut paths: Vec<PathBuf> = sessions.keys().cloned().collect();
    paths.extend(reservations.iter().map(|r| r.project.clone()));
    paths.extend(tokens.iter().map(|(p, _)| p.clone()));
    paths.extend(data.iter().map(|e| e.project.clone()));
    paths.sort();
    paths.dedup();

    let mut totals = Totals { ports_reserved: reservations.len(), ..Totals::default() };
    let mut checkouts = Vec::new();
    for path in paths {
        let session = sessions.get(&path);
        let reserved: Vec<&ports::Reservation> = reservations.iter().filter(|r| r.project == path).collect();
        let recorded: Vec<&datadirs::Entry> = data.iter().filter(|e| e.project == path).collect();
        let dir_exists = path.exists();
        let dir = match (dir_exists, session) {
            (false, _) => "missing",
            (true, Some(s)) if session::checkout_gone(s).is_some() => "replaced",
            // Only data recorded for a checkout token the directory there now does not have.
            (true, None) if !recorded.is_empty() && {
                let here = crate::checkout::token(&path);
                recorded.iter().all(|e| here.as_deref() != Some(e.checkout.as_str()))
            } => "replaced",
            _ => "present",
        };
        let size = |dir: &str| if sizes { disk_usage(Path::new(dir)) } else { None };
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
        let services: Vec<ServiceRow> = rows
            .into_values()
            .map(|mut row| {
                row.port_accepting = row.port.is_some_and(accepting);
                row.data_bytes = row.data_dir.as_deref().and_then(size);
                row
            })
            .collect();
        let live = |row: &ServiceRow| row.pid_alive.unwrap_or(row.port_accepting);
        let live_count = services.iter().filter(|r| live(r)).count();
        let state_name = match session {
            None => "reserved",
            Some(s) if s.launching => "launching",
            Some(s) if s.services.is_empty() => "recorded",
            Some(_) if live_count == services.len() => "running",
            Some(_) if live_count > 0 => "partial",
            Some(_) => "stopped",
        };
        // As `gc` decides: a missing directory with no session left.
        let reclaimable = !dir_exists && session.is_none();
        let bytes: u64 = services.iter().filter_map(|r| r.data_bytes).sum();
        totals.checkouts += 1;
        totals.running += usize::from(state_name == "running");
        if session.is_some() {
            totals.services += services.len();
            totals.services_live += live_count;
        }
        totals.data_bytes += bytes;
        if reclaimable {
            totals.reclaimable_bytes += bytes;
        }
        checkouts.push(Checkout {
            dir_exists,
            dir,
            state: state_name,
            session: session.map(|s| SessionSummary { id: s.id.clone(), started_at: s.started_at, launching: s.launching }),
            lease: session.map(|s| LeaseSummary::of(s.lease.as_ref())),
            services,
            identity_tokens: tokens.iter().filter(|(p, _)| *p == path).count(),
            reclaimable,
            path,
        });
    }
    Ok(PsReport { checks: PS_CHECKS, checkouts, totals })
}

impl ServiceRow {
    fn empty(name: &str, port: Option<u16>) -> Self {
        Self { name: name.to_string(), port, reserved_port: None, pid: None, pid_alive: None, port_accepting: false, data_dir: None, data_bytes: None }
    }
}

/// The session index: each readable record. A file another command removes between listing and
/// reading is skipped.
fn indexed_sessions(state: &Path) -> Result<Vec<Session>> {
    let Ok(entries) = fs::read_dir(state.join("sessions")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let session: Session = read_json(&path)?;
        if session.project.as_os_str().is_empty() {
            continue;
        }
        out.push(session);
    }
    Ok(out)
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
        if c.reclaimable {
            line.push_str("  reclaimable by `stack gc`");
        }
        println!("{line}");
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
    let mut session: Option<Session> =
        if old_index.exists() { Some(read_json(&old_index)?) } else { read_json::<Session>(&mirror).ok().filter(|s| s.project == old) };

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
    let (moves, settled) = plan_data_moves(session.as_ref(), own, &earlier, &old, &new, new_token.as_deref(), &mut notes)?;

    // Changes start here. Each step undoes the earlier ones if it fails.
    let (ports_moved, ports_replaced) = ports::rekey(state, &old, &new)?;
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
    if !done.is_empty() {
        notes.push(format!(
            "the provider's other state for {} stays behind; `mise daemons prune` removes state of paths that no longer exist",
            old.display()
        ));
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
    let mirror: Option<Session> = read_json::<Session>(&new.join(".stack").join("session.json")).ok();
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
    let stopped = |pitchfork: &Path, state_dir: &Path, id: &str| match mise::supervised(pitchfork, state_dir, id) {
        Ok(mise::Supervised::NotFound) => None,
        Ok(mise::Supervised::Found { status, pid, .. }) if status == "stopped" && !pid.is_some_and(pid_alive) => None,
        Ok(mise::Supervised::Found { status, .. }) => Some(format!("the supervisor reports {id} {status}")),
        Err(e) => Some(format!("the supervisor cannot confirm {id} stopped ({e})")),
    };
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
            if let (Some(provider), Some(id)) = (&session.provider, &record.provider_id) {
                reasons.extend(stopped(&provider.pitchfork, &provider.state_dir, id).map(|why| format!("{name}: {why}")));
            }
        }
    }
    for entry in own {
        if let Some(s) = &entry.supervisor {
            reasons.extend(stopped(&s.pitchfork, &s.state_dir, &s.id).map(|why| format!("{}: {why}", entry.service)));
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
fn plan_data_moves(
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
    let listed = mise::daemons_listed(new)?;
    let mut moves = Vec::new();
    for (entry, h) in held {
        let to = listed.iter().find(|d| d.name == entry.service).and_then(|d| d.data_dir.clone()).map(PathBuf::from).ok_or_else(|| {
            StackError::new("mv_data_unplaced", format!("the provider names no data directory for {} at {}", entry.service, new.display())).hint(format!(
                "run `stack compile -C {}` so its provider configuration is current, then retry `stack mv {} {}`",
                new.display(),
                old.display(),
                new.display()
            ))
        })?;
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
}
