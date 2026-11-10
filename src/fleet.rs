//! Machine-wide views across checkouts: `ps` lists what Stack knows about.
//!
//! Everything here reads the same machine state the lifecycle writes (`sessions/`, `ports.json`,
//! `identities.json`, `data.json`). `ps` only reads it, without any lock: every state file is
//! replaced atomically, so a read sees one version or the other, and `ps` never waits behind
//! `up`.

use crate::datadirs;
use crate::error::Result;
use crate::identity;
use crate::ports;
use crate::session::{self, Session};
use crate::state::{now, pid_alive, read_json};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
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
pub struct LeaseSummary {
    /// `ttl`, `owner_pid`, `ttl+owner_pid`, or `none` (lives until `stack down`).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    pub renewed_at: u64,
    /// When the TTL runs out unless renewed; absent without a TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub expired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expired_reason: Option<String>,
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
            lease: session.map(|s| lease_summary(s.lease.as_ref())),
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

fn lease_summary(lease: Option<&session::Lease>) -> LeaseSummary {
    let Some(lease) = lease else {
        return LeaseSummary { kind: "none", ttl_secs: None, owner_pid: None, renewed_at: 0, expires_at: None, expired: false, expired_reason: None };
    };
    let kind = match (lease.ttl_secs, lease.owner_pid) {
        (Some(_), Some(_)) => "ttl+owner_pid",
        (Some(_), None) => "ttl",
        (None, Some(_)) => "owner_pid",
        (None, None) => "none",
    };
    let reason = lease.expired(now());
    LeaseSummary {
        kind,
        ttl_secs: lease.ttl_secs,
        owner_pid: lease.owner_pid,
        renewed_at: lease.renewed_at,
        expires_at: lease.ttl_secs.map(|ttl| lease.renewed_at.saturating_add(ttl)),
        expired: reason.is_some(),
        expired_reason: reason,
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
    fn sizes_read_for_people() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
