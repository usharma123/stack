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
/// replaced; pinned ports are the project's to choose.
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

    reg.reservations.retain(|r| r.project.exists());
    reg.reservations.retain(|r| {
        r.project != project || (!reassign && requests.iter().any(|q| q.service == r.service))
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
            find_free(&reg, project, &req.service, ephemeral)?
        };
        reg.reservations.retain(|r| !mine(r));
        reg.reservations.push(Reservation { port, project: project.to_path_buf(), service: req.service.clone() });
        out.insert(req.service.clone(), port);
    }

    write_json(&file, &reg)?;
    Ok(out)
}

/// Release the reservations of projects whose directory is gone, except those `keep` holds
/// on to (a project whose session record is still kept: its services may still listen there).
/// Returns what was released. `assign` prunes gone projects too, but only when some project
/// compiles; GC calls this so a removed checkout's ports come back without that.
pub fn release_gone(state: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<Reservation>> {
    let (file, lock) = paths(state);
    if !file.exists() {
        return Ok(Vec::new());
    }
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    let (gone, kept): (Vec<Reservation>, Vec<Reservation>) =
        reg.reservations.into_iter().partition(|r| !r.project.exists() && !keep(&r.project));
    reg.reservations = kept;
    if !gone.is_empty() {
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

fn find_free(reg: &Registry, project: &Path, service: &str, ephemeral: &RangeInclusive<u16>) -> Result<u16> {
    let span = u32::from(RANGE_END - RANGE_START) + 1;
    // Spread projects across the range so concurrent first runs rarely race for the same port.
    let seed = sha256_hex(format!("{}\0{service}", project.display()).as_bytes());
    let offset = u32::from_str_radix(&seed[..8], 16).unwrap_or(0) % span;
    for i in 0..span {
        let port = RANGE_START + ((offset + i) % span) as u16;
        if ephemeral.contains(&port) || reg.reservations.iter().any(|r| r.port == port) {
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
                .map(|(service, port)| Reservation { port, project: project.path().to_path_buf(), service: service.to_string() })
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
}
