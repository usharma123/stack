//! Machine-wide port reservations, so independent checkouts never collide.
//!
//! Ports come from a stack-owned range rather than service defaults: a project can never
//! silently reach some other Postgres that happens to be listening on 5432.

use crate::error::{Result, StackError};
use crate::hash::sha256_hex;
use crate::state::{read_json, write_json, FileLock};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

pub const RANGE_START: u16 = 40000;
pub const RANGE_END: u16 = 49999;

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
/// keeps the same ports until `reassign` or until its directory is deleted.
pub fn assign(state: &Path, project: &Path, requests: &[Request], reassign: bool) -> Result<IndexMap<String, u16>> {
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
        } else if let Some(existing) = reg.reservations.iter().find(|r| mine(r)) {
            existing.port
        } else {
            find_free(&reg, project, &req.service)?
        };
        reg.reservations.retain(|r| !mine(r));
        reg.reservations.push(Reservation { port, project: project.to_path_buf(), service: req.service.clone() });
        out.insert(req.service.clone(), port);
    }

    write_json(&file, &reg)?;
    Ok(out)
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

fn find_free(reg: &Registry, project: &Path, service: &str) -> Result<u16> {
    let span = u32::from(RANGE_END - RANGE_START) + 1;
    // Spread projects across the range so concurrent first runs rarely race for the same port.
    let seed = sha256_hex(format!("{}\0{service}", project.display()).as_bytes());
    let offset = u32::from_str_radix(&seed[..8], 16).unwrap_or(0) % span;
    for i in 0..span {
        let port = RANGE_START + ((offset + i) % span) as u16;
        if reg.reservations.iter().any(|r| r.port == port) {
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

    #[test]
    fn lsof_fields_name_the_first_listener() {
        let out = "p4242\ncpostgres\nf5\np4300\ncredis-server\n";
        let h = parse_lsof(out).unwrap();
        assert_eq!((h.pid, h.command.as_str()), (4242, "postgres"));
        assert!(parse_lsof("").is_none());
        assert_eq!(parse_lsof("p77\n").unwrap().pid, 77);
    }
}
