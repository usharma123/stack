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
