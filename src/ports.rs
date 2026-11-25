//! Machine-wide port reservations, so independent checkouts never collide.
//!
//! Ports come from a stack-owned range rather than service defaults: a project can never
//! silently reach some other Postgres that happens to be listening on 5432.
//!
//! The range lies below every platform's default ephemeral range (Linux 32768-60999, macOS
//! and Windows 49152-65535). The system hands ports there to outgoing connections, and while
//! such a connection, or what it leaves in TIME_WAIT, holds a stopped service's port, nothing
//! can listen on it: the service fails to start though no program listens there.

use crate::error::{Result, StackError};
use crate::hash::sha256_hex;
use crate::state::{read_json, write_json, FileLock};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub const RANGE_START: u16 = 20000;
pub const RANGE_END: u16 = 29999;
const _: () = assert!(RANGE_END < 32768, "below every default ephemeral range");

/// The ports this system gives outgoing connections. Linux says, and may be configured to
/// reach into stack's range; elsewhere the default is assumed.
fn ephemeral() -> RangeInclusive<u16> {
    #[cfg(target_os = "linux")]
    if let Some(range) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|text| parse_port_range(&text))
    {
        return range;
    }
    if cfg!(target_os = "linux") {
        32768..=60999
    } else {
        49152..=65535
    }
}

/// `ip_local_port_range`: the first and last port, separated by whitespace.
#[cfg(any(target_os = "linux", test))]
fn parse_port_range(text: &str) -> Option<RangeInclusive<u16>> {
    let mut ports = text.split_whitespace().map(|p| p.parse::<u16>().ok());
    let (first, last) = (ports.next()??, ports.next()??);
    (first <= last).then_some(first..=last)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    reservations: Vec<Reservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub port: u16,
    pub project: PathBuf,
    pub service: String,
    /// Where the checkout was (`project_device`, `project_location`: see
    /// [`crate::checkout::Recorded`]); absent in reservations an earlier Stack made.
    #[serde(flatten)]
    pub project_at: crate::checkout::Recorded,
}

impl Reservation {
    /// Its checkout is shown to be gone (see [`crate::checkout::gone`]).
    pub fn checkout_gone(&self) -> bool {
        crate::checkout::gone(&self.project, &self.project_at)
    }
}

/// The process listening on a port, when the platform's tools can say without privileges.
#[derive(Debug, Clone, Serialize)]
pub struct Holder {
    pub pid: u32,
    pub command: String,
}

/// Best effort: `lsof` (macOS, most Linux distributions) names the listener of a TCP port
/// owned by the same user without privileges; Linux hosts without `lsof` are read through
/// `/proc`. Other users' processes give `None`, which means "unknown", never "free". The
/// answer only decorates a diagnostic, it never decides anything.
pub fn holder(port: u16) -> Option<Holder> {
    lsof_holder(port).or_else(|| proc_holder(port))
}

fn lsof_holder(port: u16) -> Option<Holder> {
    let mut command = std::process::Command::new("lsof");
    command.args(["-nP", "-Fpc", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]);
    let out = crate::process::capture(&mut command, std::time::Duration::from_secs(3), 64 * 1024).ok()?;
    if out.timed_out {
        return None;
    }
    parse_lsof(&out.stdout)
}

/// `/proc/net/tcp{,6}` names the listening socket's inode; `/proc/<pid>/fd` says who holds it.
#[cfg(target_os = "linux")]
fn proc_holder(port: u16) -> Option<Holder> {
    let inode = ["/proc/net/tcp", "/proc/net/tcp6"].iter().find_map(|table| {
        let text = std::fs::read_to_string(table).ok()?;
        text.lines().skip(1).find_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let local = f.get(1)?.rsplit_once(':')?.1;
            (f.get(3) == Some(&"0A") && u16::from_str_radix(local, 16).ok()? == port)
                .then(|| f.get(9)?.parse::<u64>().ok())
                .flatten()
        })
    })?;
    let target = format!("socket:[{inode}]");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|l| l.to_string_lossy() == target)) {
            let command = std::fs::read_to_string(entry.path().join("comm")).map(|c| c.trim().to_string()).unwrap_or_default();
            return Some(Holder { pid, command });
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn proc_holder(_: u16) -> Option<Holder> {
    None
}

/// `lsof -F` output: one field per line, `p<pid>` starting a process, `c<command>` following.
fn parse_lsof(output: &str) -> Option<Holder> {
    let mut pid = None;
    for line in output.lines() {
        match line.as_bytes().first() {
            Some(b'p') => pid = line[1..].parse().ok(),
            Some(b'c') => {
                if let Some(pid) = pid {
                    return Some(Holder { pid, command: line[1..].to_string() });
                }
            }
            _ => {}
        }
    }
    pid.map(|pid| Holder { pid, command: String::new() })
}

/// A service needing a port: `fixed` when the project pinned one.
pub struct Request {
    pub service: String,
    pub fixed: Option<u16>,
}

fn paths(state: &Path) -> (PathBuf, PathBuf) {
    (state.join("ports.json"), state.join("ports.lock"))
}

/// Assign (or reuse) a port per service. Reservations persist across restarts, so a project
/// keeps the same ports until `reassign` or until its directory is deleted. An assigned port
/// the system may give an outgoing connection (one reserved before the range moved) is
/// replaced; pinned ports are the project's to choose, and no other service of it is given one
/// (a service Stack gave a port the project pins since is moved to another, in whatever order
/// the services come). With `reassign` (`compile
/// --reassign-ports`) every service that is not pinned gets a new port: the ports the project
/// gives up stay excluded while new ones are chosen, as [`reassign`] keeps the old one.
pub fn assign(state: &Path, project: &Path, requests: &[Request], reassign: bool) -> Result<IndexMap<String, u16>> {
    assign_outside(state, project, requests, reassign, &ephemeral())
}

fn assign_outside(
    state: &Path,
    project: &Path,
    requests: &[Request],
    reassign: bool,
    ephemeral: &RangeInclusive<u16>,
) -> Result<IndexMap<String, u16>> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    let before = reserved(&reg);

    // Only checkouts shown to be gone: one merely out of reach (its volume not mounted, a
    // lookup refused) keeps its ports for when it is back.
    reg.reservations.retain(|r| !r.checkout_gone());
    // A command run in this checkout adopts the path's records (see
    // [`crate::checkout::Recorded`]), keeping what they knew of its mount where nothing can be
    // learned now, even for reservations given up below.
    let previous_at = reg.reservations.iter().find(|r| r.project == project && r.project_at.location.is_some()).map(|r| r.project_at.clone()).unwrap_or_default();
    let at = crate::checkout::Recorded::adopted(project, &previous_at);
    // Never chosen for a service: the ports this checkout gives up when reassigning, and those
    // any of its services pins (a pin later in `requests` is not reserved yet).
    let pins: Vec<u16> = requests.iter().filter_map(|q| q.fixed).collect();
    let mut excluded = pins.clone();
    if reassign {
        excluded.extend(reg.reservations.iter().filter(|r| r.project == project).map(|r| r.port));
    }
    // Of this checkout's reservations, only those of services it still requests are kept (none
    // when reassigning), and of those not one on a port another of its services now pins: a pin
    // configured after Stack gave that port to a service of the same checkout takes it, and that
    // service is assigned another (a pinned port is excluded from what it is chosen from).
    reg.reservations.retain(|r| {
        let wanted = requests.iter().find(|q| q.service == r.service);
        r.project != project
            || (!reassign && wanted.is_some_and(|q| !pins.contains(&r.port) || q.fixed == Some(r.port)))
    });

    let mut out = IndexMap::new();
    for req in requests {
        let mine = |r: &Reservation| r.project == project && r.service == req.service;
        let port = if let Some(port) = req.fixed {
            if let Some(other) = reg.reservations.iter().find(|r| r.port == port && !mine(r)) {
                return Err(StackError::new(
                    "port_conflict",
                    format!(
                        "service '{}' pins port {port}, already reserved for '{}' in {}",
                        req.service,
                        other.service,
                        other.project.display()
                    ),
                )
                .hint("pick another port in [override.services], or remove the pin to let stack assign one"));
            }
            port
        } else if let Some(existing) = reg.reservations.iter().find(|r| mine(r) && !ephemeral.contains(&r.port)) {
            existing.port
        } else {
            find_free(&reg, project, &req.service, ephemeral, &excluded)?
        };
        let project_at = match reg.reservations.iter().find(|r| mine(r)) {
            Some(previous) => at.clone().or_kept(&previous.project_at),
            None => at.clone(),
        };
        reg.reservations.retain(|r| !mine(r));
        reg.reservations.push(Reservation { port, project: project.to_path_buf(), service: req.service.clone(), project_at });
        out.insert(req.service.clone(), port);
    }

    // Every command of every checkout comes through here, nearly always to be given the ports
    // it already has: the registry is written (and synced) only when what it reserves changed.
    // The order of its entries, which the loop above changes, means nothing. A registry that
    // does not exist yet is still created, as it always was.
    if reserved(&reg) != before || !file.exists() {
        write_json(&file, &reg)?;
    }
    Ok(out)
}

/// What a registry reserves, whatever the order of its entries.
fn reserved(reg: &Registry) -> Vec<(PathBuf, String, u16, crate::checkout::Recorded)> {
    let mut all: Vec<(PathBuf, String, u16, crate::checkout::Recorded)> =
        reg.reservations.iter().map(|r| (r.project.clone(), r.service.clone(), r.port, r.project_at.clone())).collect();
    all.sort_unstable_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
    all
}

/// Give `services` of `project` new ports (after another program took theirs), keeping its
/// other reservations. No service is given a port any of them gave up, nor one reserved; either
/// every service moves or (no free port for one) none does. Returns `(service, old, new)` for
/// each service that had a reservation.
pub fn reassign(state: &Path, project: &Path, services: &[String]) -> Result<Vec<(String, u16, u16)>> {
    reassign_outside(state, project, services, &ephemeral())
}

fn reassign_outside(state: &Path, project: &Path, services: &[String], ephemeral: &RangeInclusive<u16>) -> Result<Vec<(String, u16, u16)>> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    // Every port these services give up stays excluded until each has its new one: once one
    // service has moved, its old port is no longer reserved, and the program that held it may
    // have let it go (it binds again), but no other service of the move is given it.
    let surrendered: Vec<u16> = reg.reservations.iter().filter(|r| r.project == project && services.contains(&r.service)).map(|r| r.port).collect();
    let mut moved = Vec::new();
    for service in services {
        let mine = |r: &Reservation| r.project == project && &r.service == service;
        let Some(old) = reg.reservations.iter().find(|r| mine(r)).map(|r| r.port) else { continue };
        let new = find_free(&reg, project, service, ephemeral, &surrendered)?;
        let project_at = reg.reservations.iter().find(|r| mine(r)).map_or_else(|| crate::checkout::Recorded::of(project), |r| crate::checkout::Recorded::adopted(project, &r.project_at));
        reg.reservations.retain(|r| !mine(r));
        reg.reservations.push(Reservation { port: new, project: project.to_path_buf(), service: service.clone(), project_at });
        moved.push((service.clone(), old, new));
    }
    // Nothing failed: every move is written at once (a failure above writes none of them).
    if !moved.is_empty() {
        write_json(&file, &reg)?;
    }
    Ok(moved)
}

/// Release the reservations of projects shown to be gone (see [`crate::checkout::presence`]),
/// except those `keep` holds on to (a project whose session record is still kept: its services
/// may still listen there). The reservations of checkouts still there are brought up to date as
/// far as observation may (see [`crate::checkout::Refresh`]): one from another boot gets the
/// mount's ids there; one naming no mount is completed by the next command run in its checkout.
/// Returns what was released. `assign` prunes gone projects too, but only when some project
/// compiles; GC calls this so a removed checkout's ports come back without that.
pub fn release_gone(state: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<Reservation>> {
    let (file, lock) = paths(state);
    if !file.exists() {
        return Ok(Vec::new());
    }
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    let (gone, mut kept): (Vec<Reservation>, Vec<Reservation>) =
        reg.reservations.into_iter().partition(|r| !keep(&r.project) && r.checkout_gone());
    let mut refresh = crate::checkout::Refresh::default();
    let mut refreshed = false;
    for r in &mut kept {
        // Observation only: a reservation records no directory to show the one there is its
        // checkout, so one naming no mount waits for a command run in it (see `assign`).
        refreshed |= refresh.observe(&r.project, &mut r.project_at, false);
    }
    reg.reservations = kept;
    if !gone.is_empty() || refreshed {
        write_json(&file, &reg)?;
    }
    Ok(gone)
}

/// Current reservations for a project, without assigning anything.
pub fn lookup(state: &Path, project: &Path) -> Result<IndexMap<String, u16>> {
    let reg: Registry = read_json(&paths(state).0)?;
    Ok(reg
        .reservations
        .into_iter()
        .filter(|r| r.project == project)
        .map(|r| (r.service, r.port))
        .collect())
}

/// Every reservation on the machine, read without taking the registry lock (writes replace the
/// file atomically). For reports only: nothing is pruned.
pub fn all(state: &Path) -> Result<Vec<Reservation>> {
    Ok(read_json::<Registry>(&paths(state).0)?.reservations)
}

/// Drop every reservation recorded for `project` (`down --purge`), returning them.
pub fn release(state: &Path, project: &Path) -> Result<Vec<Reservation>> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    let (released, kept): (Vec<Reservation>, Vec<Reservation>) = reg.reservations.into_iter().partition(|r| r.project == project);
    reg.reservations = kept;
    if !released.is_empty() {
        write_json(&file, &reg)?;
    }
    Ok(released)
}

/// Move `old`'s reservations to `new`, for a checkout moved from `old` to `new`. When `old` has
/// any, they replace whatever `new` holds (only what a compile there assigned: `stack mv`
/// refuses a `new` with a session); otherwise `new` keeps its own. Returns how many moved and
/// how many of `new`'s were replaced.
pub fn rekey(state: &Path, old: &Path, new: &Path) -> Result<(usize, usize)> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    if !reg.reservations.iter().any(|r| r.project == old) {
        return Ok((0, 0));
    }
    let before = reg.reservations.len();
    reg.reservations.retain(|r| r.project != new);
    let replaced = before - reg.reservations.len();
    let mut moved = 0;
    let at = crate::checkout::Recorded::of(new);
    for r in reg.reservations.iter_mut().filter(|r| r.project == old) {
        r.project = new.to_path_buf();
        r.project_at = at.clone();
        moved += 1;
    }
    write_json(&file, &reg)?;
    Ok((moved, replaced))
}

/// A free port for `service` of `project`: in Stack's range, outside `ephemeral`, reserved by
/// no one, not in `excluded`, and one nothing listens on now.
fn find_free(reg: &Registry, project: &Path, service: &str, ephemeral: &RangeInclusive<u16>, excluded: &[u16]) -> Result<u16> {
    let span = u32::from(RANGE_END - RANGE_START) + 1;
    // Spread projects across the range so concurrent first runs rarely race for the same port.
    let seed = sha256_hex(format!("{}\0{service}", project.display()).as_bytes());
    let offset = u32::from_str_radix(&seed[..8], 16).unwrap_or(0) % span;
    for i in 0..span {
        let port = RANGE_START + ((offset + i) % span) as u16;
        if ephemeral.contains(&port) || excluded.contains(&port) || reg.reservations.iter().any(|r| r.port == port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(StackError::new("ports_exhausted", format!("no free port in {RANGE_START}-{RANGE_END}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests(services: &[(&str, Option<u16>)]) -> Vec<Request> {
        services.iter().map(|(s, fixed)| Request { service: s.to_string(), fixed: *fixed }).collect()
    }

    #[test]
    fn reassign_moves_only_the_named_services_and_never_back_to_the_old_port() {
        let (state, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ports = assign(state.path(), project.path(), &requests(&[("web", None), ("db", None)]), false).unwrap();
        let moved = reassign(state.path(), project.path(), &["web".to_string(), "missing".to_string()]).unwrap();
        assert_eq!(moved.len(), 1);
        let (service, old, new) = &moved[0];
        assert_eq!((service.as_str(), *old), ("web", ports["web"]));
        assert_ne!(*new, ports["web"]);
        let now = lookup(state.path(), project.path()).unwrap();
        assert_eq!((now["web"], now["db"]), (*new, ports["db"]));
    }

    #[test]
    fn compile_reassign_gives_every_assigned_service_a_new_port_and_keeps_pins() {
        let (state, project, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let wanted = requests(&[("web", None), ("db", None), ("pinned", Some(40759))]);
        let before = assign(state.path(), project.path(), &wanted, false).unwrap();
        let theirs = assign(state.path(), other.path(), &requests(&[("web", None)]), false).unwrap();
        // Every port is free (nothing listens): `--reassign-ports` still moves each assigned one.
        let after = assign(state.path(), project.path(), &wanted, true).unwrap();
        for service in ["web", "db"] {
            assert_ne!(after[service], before[service], "{service} kept its port: {before:?} -> {after:?}");
            assert!(!before.values().any(|p| *p == after[service]), "{service} took a port this checkout gave up: {after:?}");
            assert_ne!(after[service], theirs["web"], "{service} took another checkout's port");
        }
        assert_eq!(after["pinned"], 40759, "a pinned port is never moved");
        assert_eq!(lookup(state.path(), project.path()).unwrap(), after);
        assert_eq!(lookup(state.path(), other.path()).unwrap(), theirs, "another checkout keeps its ports");
    }

    #[test]
    fn a_service_is_never_assigned_the_port_another_service_of_its_checkout_pins() {
        let (scratch, state, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // The port `web` of this checkout is assigned first wherever nothing else is reserved.
        let first = assign(scratch.path(), project.path(), &requests(&[("web", None)]), false).unwrap()["web"];
        // `db` pins exactly that port and comes after `web`: `web` must go elsewhere.
        let wanted = requests(&[("web", None), ("db", Some(first))]);
        for reassign in [false, true, false] {
            let ports = assign(state.path(), project.path(), &wanted, reassign).unwrap_or_else(|e| panic!("reassign {reassign}: {}", e.message));
            assert_eq!(ports["db"], first, "reassign {reassign}");
            assert_ne!(ports["web"], first, "reassign {reassign}");
        }
    }

    /// A pin configured after Stack assigned that port to another service of the same checkout:
    /// an ordinary compile (no `--reassign-ports`) takes it for the pinned service and moves the
    /// other, whichever comes first. A pin on another checkout's port is still a conflict, and a
    /// compile that fails on one changes nothing, the move of its own service included.
    #[test]
    fn a_pin_configured_over_a_port_its_own_checkout_was_assigned_takes_it_in_either_order() {
        for pinned_first in [false, true] {
            let (state, project, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let (state, project) = (state.path(), project.path());
            let ordered = |pinned: (&'static str, u16), rest: &[&'static str]| {
                let mut all: Vec<(&str, Option<u16>)> = rest.iter().map(|s| (*s, None)).collect();
                all.insert(if pinned_first { 0 } else { all.len() }, (pinned.0, Some(pinned.1)));
                requests(&all)
            };
            let ctx = format!("pinned first: {pinned_first}");
            let before = assign(state, project, &requests(&[("web", None), ("db", None)]), false).unwrap();
            let theirs = assign(state, other.path(), &requests(&[("api", None)]), false).unwrap()["api"];
            let wanted = ordered(("db", before["web"]), &["web"]);
            let after = assign(state, project, &wanted, false).unwrap_or_else(|e| panic!("{ctx}: {}: {}", e.code, e.message));
            assert_eq!(after["db"], before["web"], "{ctx}");
            assert!(![before["web"], theirs].contains(&after["web"]), "{ctx}: {before:?} -> {after:?}");
            assert_eq!(lookup(state, project).unwrap(), after, "{ctx}");
            assert_eq!(lookup(state, other.path()).unwrap()["api"], theirs, "{ctx}");
            // Stable from then on.
            assert_eq!(assign(state, project, &wanted, false).unwrap(), after, "{ctx}");
            // Pinning `db` to `web`'s port again while `cache` pins the other checkout's: refused,
            // and `web` keeps its port.
            let registry = std::fs::read(paths(state).0).unwrap();
            let mut conflict = ordered(("db", after["web"]), &["web"]);
            conflict.push(Request { service: "cache".into(), fixed: Some(theirs) });
            let e = assign(state, project, &conflict, false).unwrap_err();
            assert_eq!(e.code, "port_conflict", "{ctx}: {}", e.message);
            assert!(e.message.contains("'api'"), "{ctx}: {}", e.message);
            assert_eq!(std::fs::read(paths(state).0).unwrap(), registry, "{ctx}: a failed compile changed the registry");
            // Two services of one checkout pinning one port still conflict.
            let twice = requests(&[("web", Some(after["db"])), ("db", Some(after["db"]))]);
            assert_eq!(assign(state, project, &twice, false).unwrap_err().code, "port_conflict", "{ctx}");
            assert_eq!(std::fs::read(paths(state).0).unwrap(), registry, "{ctx}");
        }
    }

    /// `up --reassign-ports` moving several services: no service is given a port another of them
    /// gave up, though nothing holds it once its service has moved; with no fresh port for one,
    /// none moves.
    #[test]
    fn up_reassigning_several_services_never_gives_one_a_port_another_gave_up() {
        // Outgoing connections may take all but the 20 lowest ports (above the eight the seeded
        // test uses), and another checkout holds the lowest `blocked` of those.
        let ephemeral = RANGE_START + 20..=RANGE_END;
        let services = vec!["web".to_string(), "db".to_string()];
        for blocked in [17, 16] {
            let (state, project, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let (state, project) = (state.path(), project.path());
            let held = (RANGE_START..RANGE_START + blocked)
                .map(|port| Reservation { port, project: other.path().to_path_buf(), service: format!("s{port}"), project_at: Default::default() })
                .collect();
            write_json(&paths(state).0, &Registry { reservations: held }).unwrap();
            let before = assign_outside(state, project, &requests(&[("web", None), ("db", None)]), false, &ephemeral).unwrap();
            let registry = std::fs::read(paths(state).0).unwrap();
            match reassign_outside(state, project, &services, &ephemeral) {
                // Three ports free of the other checkout: one left for two services.
                Err(e) if blocked == 17 => {
                    assert_eq!(e.code, "ports_exhausted", "{}", e.message);
                    assert_eq!(std::fs::read(paths(state).0).unwrap(), registry, "a failed move changed the registry");
                }
                Ok(moved) if blocked == 16 => {
                    let new: Vec<u16> = moved.iter().map(|(_, _, new)| *new).collect();
                    assert_eq!(moved.iter().map(|(s, old, _)| (s.as_str(), *old)).collect::<Vec<_>>(), [("web", before["web"]), ("db", before["db"])]);
                    assert!(new.iter().all(|p| !before.values().any(|old| old == p)) && new[0] != new[1], "{before:?} -> {moved:?}");
                    assert_eq!(lookup(state, project).unwrap(), IndexMap::from([("web".to_string(), new[0]), ("db".to_string(), new[1])]));
                }
                other => panic!("{blocked} blocked: {other:?}"),
            }
        }
    }

    #[test]
    fn ports_come_from_outside_the_ports_outgoing_connections_take() {
        let (state, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Configured to cover all but the top of stack's range.
        let ephemeral = RANGE_START..=RANGE_END - 2;
        let ports = assign_outside(state.path(), project.path(), &requests(&[("a", None), ("b", None)]), false, &ephemeral).unwrap();
        let mut got: Vec<u16> = ports.values().copied().collect();
        got.sort_unstable();
        assert_eq!(got, [RANGE_END - 1, RANGE_END]);
        let more = assign_outside(state.path(), project.path(), &requests(&[("a", None), ("b", None), ("c", None)]), false, &ephemeral);
        assert_eq!(more.unwrap_err().code, "ports_exhausted");
    }

    #[test]
    fn a_reserved_port_outgoing_connections_may_take_is_replaced_and_a_pinned_one_kept() {
        let (state, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // As reserved before the range moved: inside Linux's ephemeral range.
        let old = Registry {
            reservations: ["db", "pinned"]
                .iter()
                .zip([40758, 40759])
                .map(|(service, port)| Reservation { port, project: project.path().to_path_buf(), service: service.to_string(), project_at: Default::default() })
                .collect(),
        };
        write_json(&paths(state.path()).0, &old).unwrap();
        let wanted = requests(&[("db", None), ("pinned", Some(40759))]);
        let ports = assign_outside(state.path(), project.path(), &wanted, false, &(32768..=60999)).unwrap();
        assert!((RANGE_START..=RANGE_END).contains(&ports["db"]), "{ports:?}");
        assert_eq!(ports["pinned"], 40759);
        // Kept from then on.
        assert_eq!(assign_outside(state.path(), project.path(), &wanted, false, &(32768..=60999)).unwrap(), ports);
        assert_eq!(lookup(state.path(), project.path()).unwrap(), ports);
    }

    #[test]
    fn the_configured_ephemeral_range_is_read_as_the_kernel_writes_it() {
        assert_eq!(parse_port_range("32768\t60999\n"), Some(32768..=60999));
        assert_eq!(parse_port_range("1024 65535"), Some(1024..=65535));
        for bad in ["", "32768", "60999 32768", "x 1", "1 70000"] {
            assert_eq!(parse_port_range(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn lsof_fields_name_the_first_listener() {
        let out = "p4242\ncpostgres\nf5\np4300\ncredis-server\n";
        let h = parse_lsof(out).unwrap();
        assert_eq!((h.pid, h.command.as_str()), (4242, "postgres"));
        assert!(parse_lsof("").is_none());
        assert_eq!(parse_lsof("p77\n").unwrap().pid, 77);
    }

    /// A deterministic generator for the state-machine tests below: the same sequence on every
    /// host, with no dependency.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// A directory made to refuse lookups (a checkout out of reach), reachable again when dropped.
    struct Refused(PathBuf);

    impl Refused {
        fn new(dir: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            Refused(dir.to_path_buf())
        }
    }

    impl Drop for Refused {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// Every registry operation (compile with and without `--reassign-ports`, `up
    /// --reassign-ports` of one service or two, `down --purge`, `gc`), interleaved across three
    /// checkouts in a seeded order, with a foreign listener coming and going, a checkout going out
    /// of reach and back, one deleted, and one whose pin is configured and removed again (its
    /// services coming in either order). After every step the real registry keeps what the port
    /// model (verify-stack-ports-mcp) promises: one reservation per port, and each checkout still
    /// there (out of reach or not) keeps exactly the ports it was last given; each assignment
    /// reuses a port (unless its own checkout now pins it), or (reassigning) never gives back any
    /// port the checkout gave up, never takes a pinned port another checkout holds, never one a
    /// foreign program listens on, and never one outgoing connections may take; a pin fails only
    /// on another checkout's reservation; a failed assignment changes nothing; a deleted
    /// checkout's ports are released by the next compile or `gc`. Only eight ports are free, so
    /// collisions and exhaustion happen.
    #[test]
    fn a_seeded_sequence_of_registry_operations_keeps_every_reservation_promise() {
        use std::collections::{BTreeMap, BTreeSet};
        // SAFETY: geteuid has no preconditions.
        let can_refuse = unsafe { libc::geteuid() } != 0;
        // Outgoing connections may take all but the bottom eight ports (the top ones are left to
        // `ports_come_from_outside_the_ports_outgoing_connections_take`, which runs alongside).
        let ephemeral = RANGE_START + 8..=RANGE_END;
        let window: Vec<u16> = (RANGE_START..RANGE_START + 8).collect();
        // The first port `find_free` reaches in the window, so a service of any checkout may hold it
        // when checkout 1 pins it.
        let pin = RANGE_START;
        // Checkout 1 pins `db` once its pin is configured, its services in either order.
        let wanted = |c: usize, pin_on: bool, db_first: bool| match c {
            0 => requests(&[("web", None), ("db", None)]),
            1 => {
                let db = ("db", pin_on.then_some(pin));
                requests(&if db_first { [db, ("web", None)] } else { [("web", None), db] })
            }
            _ => requests(&[("web", None)]),
        };
        let sorted = |ports: &IndexMap<String, u16>| ports.iter().map(|(s, p)| (s.clone(), *p)).collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        for seed in 1..=8u64 {
            let tmp = tempfile::tempdir().unwrap();
            let state = tmp.path().join("state");
            let parents: Vec<PathBuf> = (0..3).map(|i| tmp.path().join(format!("p{i}"))).collect();
            let roots: Vec<PathBuf> = parents.iter().map(|p| p.join("wt")).collect();
            for (parent, root) in parents.iter().zip(&roots) {
                std::fs::create_dir_all(root).unwrap();
                std::fs::write(parent.join("sibling"), b"").unwrap();
            }
            let registry = || all(&state).unwrap().into_iter().map(|r| (r.project, r.service, r.port)).collect::<BTreeSet<_>>();
            let mut given: Vec<IndexMap<String, u16>> = vec![IndexMap::new(); 3];
            // What earlier commands left: in half the runs, checkout 0's `db` holds a port
            // reserved before the range moved (where outgoing connections may now take one); in
            // half, checkout 1's `web` holds the port its checkout now pins.
            let mut history = Vec::new();
            for (on, c, service, port) in [(seed % 2 == 1, 0, "db", RANGE_START + 100), (seed % 4 >= 2, 1, "web", pin)] {
                if on {
                    history.push(Reservation { port, project: roots[c].clone(), service: service.into(), project_at: Default::default() });
                    given[c].insert(service.into(), port);
                }
            }
            if !history.is_empty() {
                write_json(&paths(&state).0, &Registry { reservations: history }).unwrap();
            }
            let mut deleted = [false; 3];
            let mut foreign: Option<(u16, TcpListener)> = None;
            // Dropped before `tmp`, so every directory is reachable again for its removal.
            let mut refused: Vec<Option<Refused>> = vec![None, None, None];
            let mut pin_on = seed % 4 != 1;
            let pinned = |c: usize, service: &str, pin_on: bool| pin_on && c == 1 && service == "db";
            let mut rng = Lcg(seed);
            for step in 0..300 {
                let (c, op) = (rng.below(3) as usize, rng.below(11));
                let usable = !deleted[c] && refused[c].is_none();
                let ctx = format!("seed {seed} step {step} op {op} checkout {c}");
                let before = registry();
                let mut prunes = false;
                match op {
                    0..=3 if usable => {
                        let reassign = op == 3;
                        // By the step, not the generator: its next draw follows the last too closely.
                        let db_first = step % 2 == 0;
                        match assign_outside(&state, &roots[c], &wanted(c, pin_on, db_first), reassign, &ephemeral) {
                            Ok(ports) => {
                                for (service, port) in &ports {
                                    if pinned(c, service, pin_on) {
                                        assert_eq!(*port, pin, "{ctx}");
                                        continue;
                                    }
                                    assert!((RANGE_START..=RANGE_END).contains(port) && !ephemeral.contains(port), "{ctx}: {service} got {port}");
                                    assert!(!(c == 1 && pin_on && *port == pin), "{ctx}: {service} got its checkout's pinned port");
                                    let old = given[c].get(service).copied();
                                    if reassign {
                                        assert!(!given[c].values().any(|o| o == port), "{ctx}: reassigning gave {service} back {port}: {:?}", given[c]);
                                        seen.insert("reassign");
                                    } else if c == 1 && pin_on && old == Some(pin) {
                                        seen.insert(if db_first { "pin over own reservation, pinned first" } else { "pin over own reservation, pinned last" });
                                    } else if let Some(old) = old.filter(|o| !ephemeral.contains(o)) {
                                        assert_eq!(*port, old, "{ctx}: {service} moved without reassigning");
                                        seen.insert("reuse");
                                    } else if old.is_some() {
                                        seen.insert("replaced in the ephemeral range");
                                    }
                                    if old != Some(*port) {
                                        assert!(foreign.as_ref().map_or(true, |(f, _)| f != port), "{ctx}: {service} got the port a foreign program listens on");
                                    }
                                }
                                given[c] = ports;
                                prunes = true;
                            }
                            Err(e) => {
                                assert_eq!(registry(), before, "{ctx}: a failed assignment changed the registry");
                                match e.code {
                                    "port_conflict" => {
                                        let theirs = before.iter().any(|(p, _, port)| *port == pin && p != &roots[1]);
                                        assert!(c == 1 && pin_on && theirs, "{ctx}: {}", e.message);
                                        seen.insert("pin conflict");
                                    }
                                    "ports_exhausted" => {
                                        seen.insert("exhausted");
                                    }
                                    other => panic!("{ctx}: {other}: {}", e.message),
                                }
                            }
                        }
                    }
                    // `up --reassign-ports` after a conflict: only services Stack assigned, one
                    // or (checkout 0) both at once.
                    4 if usable => {
                        let services: Vec<String> = match (c, rng.below(3)) {
                            (0, 0) => vec!["db".into()],
                            (0, 1) => vec!["db".into(), "web".into()],
                            _ => vec!["web".into()],
                        };
                        let had: Vec<(String, u16)> = services.iter().filter_map(|s| Some((s.clone(), *given[c].get(s)?))).collect();
                        match reassign_outside(&state, &roots[c], &services, &ephemeral) {
                            Ok(moved) => {
                                assert_eq!(moved.iter().map(|(s, old, _)| (s.clone(), *old)).collect::<Vec<_>>(), had, "{ctx}");
                                for (service, _, new) in &moved {
                                    assert!(!ephemeral.contains(new) && !before.iter().any(|(_, _, p)| p == new), "{ctx}: moved {service} onto {new}, given up or reserved: {had:?}");
                                    given[c].insert(service.clone(), *new);
                                }
                                if !moved.is_empty() {
                                    seen.insert(if moved.len() > 1 { "up reassign of two" } else { "up reassign" });
                                }
                            }
                            Err(e) => {
                                assert_eq!(e.code, "ports_exhausted", "{ctx}: {}", e.message);
                                assert_eq!(registry(), before, "{ctx}: a failed move changed the registry");
                                seen.insert("up reassign exhausted");
                            }
                        }
                    }
                    5 if usable => {
                        release(&state, &roots[c]).unwrap();
                        given[c].clear();
                    }
                    6 => {
                        release_gone(&state, |_| false).unwrap();
                        prunes = true;
                    }
                    7 if can_refuse && !deleted[c] => {
                        refused[c] = match refused[c].take() {
                            Some(_) => None,
                            None => {
                                seen.insert("out of reach");
                                Some(Refused::new(&parents[c]))
                            }
                        };
                    }
                    8 if c == 2 && usable && step > 100 => {
                        std::fs::remove_dir_all(&roots[c]).unwrap();
                        deleted[c] = true;
                        seen.insert("deleted");
                    }
                    9 => {
                        foreign = match foreign.take() {
                            Some(_) => None,
                            None => {
                                let port = window[rng.below(window.len() as u64) as usize];
                                TcpListener::bind(("127.0.0.1", port)).ok().map(|l| (port, l))
                            }
                        };
                    }
                    // Checkout 1's pin configured, or removed (its next compile adopts that).
                    10 => {
                        pin_on = !pin_on;
                        seen.insert(if pin_on { "pin configured" } else { "pin removed" });
                    }
                    _ => continue,
                }
                let now = all(&state).unwrap();
                let ports: BTreeSet<u16> = now.iter().map(|r| r.port).collect();
                assert_eq!(ports.len(), now.len(), "{ctx}: a port reserved twice: {now:?}");
                for i in 0..3 {
                    let kept = lookup(&state, &roots[i]).unwrap();
                    if deleted[i] {
                        if prunes {
                            assert!(kept.is_empty(), "{ctx}: deleted checkout {i} kept {kept:?}");
                            seen.insert("released");
                        }
                    } else {
                        assert_eq!(sorted(&kept), sorted(&given[i]), "{ctx}: checkout {i} (out of reach: {})", refused[i].is_some());
                    }
                }
            }
        }
        let mut wanted = vec![
            "reassign",
            "reuse",
            "pin conflict",
            "pin configured",
            "pin over own reservation, pinned first",
            "pin over own reservation, pinned last",
            "up reassign",
            "up reassign of two",
            "replaced in the ephemeral range",
            "deleted",
            "released",
        ];
        if can_refuse {
            wanted.push("out of reach");
        }
        for case in wanted {
            assert!(seen.contains(case), "no run reached {case}: {seen:?}");
        }
    }

    /// Checkouts compiling at once, each in its own thread, through the real registry lock:
    /// whatever the interleaving, no two share a port, and each keeps what it was last given.
    #[test]
    fn checkouts_assigning_at_once_never_share_a_port_and_each_keeps_what_it_was_given() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let roots: Vec<PathBuf> = (0..8).map(|i| tmp.path().join(format!("wt{i}"))).collect();
        for root in &roots {
            std::fs::create_dir_all(root).unwrap();
        }
        let start = std::sync::Barrier::new(roots.len());
        let last: Vec<IndexMap<String, u16>> = std::thread::scope(|scope| {
            let workers: Vec<_> = roots
                .iter()
                .map(|root| {
                    let (state, start) = (&state, &start);
                    scope.spawn(move || {
                        start.wait();
                        let mut last = IndexMap::new();
                        for round in 0..5 {
                            last = assign(state, root, &requests(&[("web", None), ("db", None)]), round == 2).unwrap();
                        }
                        last
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        let ports: std::collections::BTreeSet<u16> = last.iter().flat_map(|p| p.values().copied()).collect();
        assert_eq!(ports.len(), roots.len() * 2, "{last:?}");
        for (root, given) in roots.iter().zip(&last) {
            assert_eq!(&lookup(&state, root).unwrap(), given);
        }
        assert_eq!(all(&state).unwrap().len(), roots.len() * 2);
    }
}
