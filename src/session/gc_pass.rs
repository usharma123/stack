//! How a GC pass chooses what to lock, so that one checkout's `up` never waits on, or fails
//! because of, another checkout, what `gc --dry-run` reports, and the opt-in stop of a deleted
//! checkout's services (`gc --stop-orphans`).

use super::{accepting, checkout_state, gc_locked, index_path, is_unreadable, session_record, CheckoutState, GcEntry, ProviderRecord, ServiceRecord, Session, Steps, Unconfirmed, STOP_TIMEOUT};
use crate::error::{Result, StackError};
use crate::provider::mise;
use crate::state::{now, pid_alive, project_lock, project_try_lock, rfc3339};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How a GC pass treats other projects.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcOptions {
    /// The pass `up` runs first: never wait for another project's lock (a busy candidate is
    /// skipped and reported), and never fail (problems become warnings).
    pub background: bool,
    /// Stop a deleted checkout's service when every recorded fact still matches it.
    pub stop_orphans: bool,
}

/// What one pass did.
#[derive(Debug, Default, Serialize)]
pub struct GcPass {
    pub reclaimed: Vec<GcEntry>,
    /// Candidates another command held locked; left for a later pass.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<GcSkipped>,
    /// Problems a background pass did not fail on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Checkouts whose session record could not be read (see `session_record`): reported, never
    /// stopped, reclaimed or released.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<GcEntry>,
}

/// The `reason` of a [`GcEntry`] for an unreadable session record.
pub const UNREADABLE: &str = "session record unreadable";

/// The report for an unreadable record: the checkout when it is known, else the record.
fn unreadable_entry(project: &Path, error: &StackError) -> GcEntry {
    GcEntry { project: project.to_path_buf(), reason: UNREADABLE.into(), stopped: false, error: Some(error.to_string()), services: Vec::new(), left_alone: false }
}

/// What one checkout's index entry at `path` holds, both copies considered: the indexed
/// session when it and the project copy can be read; `Err` (the checkout, when known, and why)
/// when either cannot. A deleted checkout has no copy to read.
pub(crate) fn readable_session(state: &Path, path: &Path) -> Result<std::result::Result<Option<Session>, (PathBuf, StackError)>> {
    let indexed = match session_record(path) {
        Ok(Some(indexed)) => indexed,
        Ok(None) => return Ok(Ok(None)),
        Err(e) if is_unreadable(&e) => {
            let checkout = super::checkout_of_record(state, path);
            let e = match &checkout {
                Some(c) => super::session_record_of(path, Some(c)).err().unwrap_or(e),
                None => e,
            };
            return Ok(Err((checkout.unwrap_or_else(|| path.to_path_buf()), e)));
        }
        Err(e) => return Err(e),
    };
    if indexed.project.exists() {
        match super::session_record_of(&indexed.project.join(".stack").join("session.json"), Some(&indexed.project)) {
            Err(e) if is_unreadable(&e) => return Ok(Err((indexed.project.clone(), e))),
            Err(e) => return Err(e),
            Ok(_) => {}
        }
    }
    Ok(Ok(Some(indexed)))
}

#[derive(Debug, Serialize)]
pub struct GcSkipped {
    pub project: PathBuf,
    pub reason: String,
    /// Why it was not reclaimed now: `busy` (another stack command holds its lock), or
    /// `unavailable` (whether the checkout is still there cannot be shown: a lookup refused or
    /// failing, or its volume may not be mounted; `reason` says which).
    pub skipped: &'static str,
}

/// Why `session` may be reclaimed, judged from what it records without taking its lock. A
/// session that is not a candidate is never locked; one that is gets checked again under
/// the lock, since another command may renew or replace it meanwhile.
pub(super) fn candidate(session: &Session, checkout: &CheckoutState, now: u64) -> Option<String> {
    match checkout {
        CheckoutState::Gone(reason) => Some((*reason).to_string()),
        CheckoutState::Unavailable(_) => None,
        CheckoutState::Present => session.lease.as_ref().and_then(|l| l.expired(now)),
    }
}

/// The report for a checkout whose presence cannot be shown (see [`CheckoutState`]): left for
/// a later pass, as a busy one is.
fn unavailable(session: &Session, checkout: &CheckoutState) -> Option<GcSkipped> {
    match checkout {
        CheckoutState::Unavailable(why) => Some(GcSkipped { project: session.project.clone(), reason: why.clone(), skipped: "unavailable" }),
        _ => None,
    }
}

/// The machine index's session records, in a stable order.
pub(super) fn index_files(state: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(state.join("sessions")) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
}

/// One GC pass over every indexed session. Only candidates are locked: a standalone pass
/// waits for a busy one, a background pass skips it. A background pass also turns every
/// failure (an unreadable record, a lock or stop error, a session it could not confirm
/// stopped) into a warning instead of failing.
pub fn gc_with(state: &Path, opts: &GcOptions) -> Result<GcPass> {
    let mut pass = GcPass::default();
    for path in index_files(state) {
        let outcome = (|| -> Result<()> {
            // An unreadable record (the index's, or the checkout's own copy), whether or not the
            // checkout looks reclaimable, is reported, that checkout left alone, and the pass
            // goes on.
            let indexed = match readable_session(state, &path)? {
                Ok(Some(indexed)) => indexed,
                Ok(None) => return Ok(()),
                Err((checkout, e)) => {
                    pass.unreadable.push(unreadable_entry(&checkout, &e));
                    return Ok(());
                }
            };
            let checkout = checkout_state(&indexed);
            if let Some(skipped) = unavailable(&indexed, &checkout) {
                pass.skipped.push(skipped);
                return Ok(());
            }
            let Some(reason) = candidate(&indexed, &checkout, now()) else { return Ok(()) };
            let _guard = if opts.background {
                match project_try_lock(state, &indexed.project)? {
                    Some(guard) => guard,
                    None => {
                        pass.skipped.push(GcSkipped { project: indexed.project, reason, skipped: "busy" });
                        return Ok(());
                    }
                }
            } else {
                project_lock(state, &indexed.project)?
            };
            let locked = match gc_locked(state, &path, opts.stop_orphans) {
                Err(e) if is_unreadable(&e) => {
                    pass.unreadable.push(unreadable_entry(&indexed.project, &e));
                    return Ok(());
                }
                other => other?,
            };
            if let Some(entry) = locked {
                if opts.background && !entry.stopped {
                    pass.warnings.push(format!(
                        "{}: {} but not confirmed stopped ({}); run `stack gc` for details",
                        entry.project.display(),
                        entry.reason,
                        entry.error.as_deref().unwrap_or("unknown")
                    ));
                }
                pass.reclaimed.push(entry);
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {}
            Err(e) if opts.background => pass.warnings.push(format!("{}: {e}", path.display())),
            Err(e) => return Err(e),
        }
    }
    Ok(pass)
}

/// The pass at the start of every `up`: it never waits for, and never fails because of,
/// another checkout. Recorded as the `gc` step (a warning when anything went wrong, listing
/// what it skipped when anything was busy); returns what it reclaimed and the warnings for
/// the result.
pub(super) fn before_up(state: &Path, steps: &mut Steps) -> (Vec<GcEntry>, Vec<String>) {
    let mut pass = gc_with(state, &GcOptions { background: true, stop_orphans: false }).unwrap_or_else(|e| GcPass {
        warnings: vec![e.to_string()],
        ..GcPass::default()
    });
    for entry in std::mem::take(&mut pass.unreadable) {
        pass.warnings.push(format!("{}: {}; left alone", entry.project.display(), entry.error.unwrap_or_default()));
    }
    let mut detail = serde_json::json!({ "reaped": pass.reclaimed.len() });
    if !pass.skipped.is_empty() {
        detail["skipped"] = serde_json::json!(pass.skipped);
    }
    if pass.warnings.is_empty() {
        steps.ok("gc", detail);
    } else {
        detail["warnings"] = serde_json::json!(pass.warnings);
        steps.warn("gc", detail);
    }
    let warnings = pass.warnings.into_iter().map(|w| format!("gc: {w}")).collect();
    (pass.reclaimed, warnings)
}

/// What `gc --dry-run` reports for one session.
#[derive(Debug, Serialize)]
pub struct GcPlanned {
    pub session: String,
    pub project: PathBuf,
    pub reason: String,
    /// `stop` (`stack down` for an expired lease) or `release_if_stopped` (a deleted or
    /// replaced checkout: its record is dropped once the supervisor confirms each service
    /// stopped; nothing is stopped).
    pub action: &'static str,
    /// A command was executing in it when read; GC leaves a busy session alone (unless its
    /// owner process exited).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    pub services: Vec<PlannedService>,
}

#[derive(Debug, Serialize)]
pub struct PlannedService {
    pub service: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Whether the recorded pid is alive now (possibly reused by another process).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid_alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// What a deleted checkout left that `gc` would release or reclaim: its port reservations and
/// the data directories `up` recorded for it.
#[derive(Debug, Serialize)]
pub struct DeletedPlan {
    pub project: PathBuf,
    /// A session record of it is still kept: the rest goes only once `gc` drops that record
    /// (its services confirmed stopped).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub after_session: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PlannedPort>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<PlannedData>,
}

#[derive(Debug, Serialize)]
pub struct PlannedPort {
    pub service: String,
    pub port: u16,
}

#[derive(Debug, Serialize)]
pub struct PlannedData {
    pub service: String,
    pub dir: PathBuf,
    /// `remove_if_safe` with `--reclaim-data` (only after the checks `gc` makes then: marker,
    /// directory identity, supervisor, no process carrying the checkout's token), else `report`.
    pub action: &'static str,
}

#[derive(Debug, Serialize)]
pub struct GcDryRun {
    pub dry_run: bool,
    pub would_reclaim: Vec<GcPlanned>,
    /// Checkouts whose session record cannot be read: a pass reports them and leaves them alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<GcEntry>,
    /// Checkouts whose presence cannot be shown (see [`GcSkipped`]): a pass leaves them alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<GcSkipped>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_checkouts: Vec<DeletedPlan>,
    /// Postgres templates nothing can use any more (see [`crate::pgtemplate`]), which a pass
    /// removes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<crate::pgtemplate::Unused>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `gc --dry-run`: what a pass would try to reclaim, and what deleted checkouts would have
/// released (or, with `reclaim_data`, removed), read without any lock and changing nothing
/// (completion records are not applied, no supervisor is asked, no data directory opened).
pub fn gc_plan(state: &Path, reclaim_data: bool) -> Result<GcDryRun> {
    let mut planned = Vec::new();
    let mut warnings = Vec::new();
    let mut unreadable = Vec::new();
    let mut unavailable_checkouts = Vec::new();
    let now = now();
    for path in index_files(state) {
        // As the pass decides it (see `gc_with`): both copies, before anything is planned.
        let session: Session = match readable_session(state, &path) {
            Ok(Ok(Some(s))) => s,
            Ok(Ok(None)) => continue,
            Ok(Err((checkout, e))) => {
                unreadable.push(unreadable_entry(&checkout, &e));
                continue;
            }
            Err(e) => {
                warnings.push(e.to_string());
                continue;
            }
        };
        let checkout = checkout_state(&session);
        if let Some(skipped) = unavailable(&session, &checkout) {
            unavailable_checkouts.push(skipped);
            continue;
        }
        let Some(reason) = candidate(&session, &checkout, now) else { continue };
        let owner_dead = session.lease.as_ref().is_some_and(super::Lease::owner_exited);
        let busy = !owner_dead && session.active_executions.values().any(|pid| pid_alive(*pid));
        planned.push(GcPlanned {
            session: session.id.clone(),
            action: if checkout.gone().is_some() { "release_if_stopped" } else { "stop" },
            project: session.project.clone(),
            reason,
            busy,
            services: session
                .services
                .iter()
                .map(|(name, r)| PlannedService {
                    service: name.clone(),
                    port: r.port,
                    pid: r.pid,
                    pid_alive: r.pid.map(pid_alive),
                    provider_id: r.provider_id.clone(),
                })
                .collect(),
        });
    }
    let mut deleted: Vec<DeletedPlan> = Vec::new();
    let mut entry = |project: &Path| -> usize {
        if let Some(i) = deleted.iter().position(|d| d.project == project) {
            return i;
        }
        let after_session = index_path(state, project).exists();
        deleted.push(DeletedPlan { project: project.to_path_buf(), after_session, ports: Vec::new(), data: Vec::new() });
        deleted.len() - 1
    };
    let mut ports = Vec::new();
    match crate::ports::all(state) {
        Ok(all) => ports = all,
        Err(e) => warnings.push(e.to_string()),
    }
    let mut data = Vec::new();
    match crate::datadirs::entries(state) {
        Ok(all) => data = all,
        Err(e) => warnings.push(e.to_string()),
    }
    let mut found: Vec<(usize, Option<PlannedPort>, Option<PlannedData>)> = Vec::new();
    // Only checkouts shown to be gone; one whose presence cannot be shown is reported, as a pass
    // leaves its ports and data alone.
    let mut gone = |project: &Path, at: &crate::checkout::Recorded| match crate::checkout::presence(project, at) {
        crate::checkout::Presence::Absent => true,
        crate::checkout::Presence::Present => false,
        crate::checkout::Presence::Unknown(reason) => {
            if !unavailable_checkouts.iter().any(|u: &GcSkipped| u.project == project) {
                unavailable_checkouts.push(GcSkipped { project: project.to_path_buf(), reason, skipped: "unavailable" });
            }
            false
        }
    };
    for r in ports {
        if gone(&r.project, &r.project_at) {
            found.push((entry(&r.project), Some(PlannedPort { service: r.service, port: r.port }), None));
        }
    }
    let action = if reclaim_data { "remove_if_safe" } else { "report" };
    for d in data {
        if gone(&d.project, &d.project_at) {
            found.push((entry(&d.project), None, Some(PlannedData { service: d.service, dir: d.dir, action })));
        }
    }
    for (i, port, data) in found {
        deleted[i].ports.extend(port);
        deleted[i].data.extend(data);
    }
    Ok(GcDryRun { dry_run: true, would_reclaim: planned, unreadable, unavailable: unavailable_checkouts, deleted_checkouts: deleted, templates: crate::pgtemplate::collect(state, true), warnings })
}

/// How long before the recorded start a process may appear to have started: the recorded
/// time is taken just before the supervisor's start call, and Linux reports boot time in
/// whole seconds.
const START_SLACK_BEFORE: Duration = Duration::from_secs(2);
/// How long after the recorded start the process may have started: the supervisor's start
/// call, which spawns it, runs within the default startup timeout.
const START_SLACK_AFTER: Duration = super::DEFAULT_STARTUP_TIMEOUT;

/// Whether a process that started at `actual` (Unix ms) can be the one recorded as started
/// at `recorded`.
fn start_matches(recorded: u64, actual: u64) -> bool {
    actual.saturating_add(START_SLACK_BEFORE.as_millis() as u64) >= recorded
        && actual <= recorded.saturating_add(START_SLACK_AFTER.as_millis() as u64)
}

/// `gc --stop-orphans` for a deleted checkout's service whose supervisor still reports the
/// recorded id, pid and port (`refused` says so, with its recovery commands). Stops it only
/// when the live process's start time also matches the recorded one and a second supervisor
/// query, just before the stop, still reports the same id, pid and port. Then waits until the
/// pid is gone and the port closed. Anything else keeps the record, as without the flag.
pub(super) fn stop_orphan(provider: Option<&ProviderRecord>, record: &ServiceRecord, refused: Unconfirmed) -> std::result::Result<String, Unconfirmed> {
    let Unconfirmed { recovery, .. } = refused;
    let keep = |why: String| Unconfirmed { why: format!("--stop-orphans: {why}; not signalled"), recovery: recovery.clone() };
    let (Some((provider, bin)), Some(id), Some(pid)) = (provider.and_then(|p| Some((p, p.legacy()?))), record.provider_id.as_deref(), record.pid) else {
        return Err(keep("the record names no supervisor, id or pid".into()));
    };
    let Some(recorded) = record.started_at_ms else {
        return Err(keep(format!("no start time was recorded for pid {pid} (a record from before Stack 0.1.18)")));
    };
    let Some(actual) = crate::procinfo::process(pid).and_then(|p| p.started_unix_ms()) else {
        return Err(keep(format!("the start time of pid {pid} cannot be read")));
    };
    if !start_matches(recorded, actual) {
        return Err(keep(format!(
            "pid {pid} started at {}, not when the record says ({}): the pid may have been reused",
            rfc3339(actual / 1000),
            rfc3339(recorded / 1000)
        )));
    }
    match mise::supervised(bin, &provider.state_dir, id) {
        Ok(mise::Supervised::Found { status, pid: Some(now), port: Some(port) }) if status == "running" && now == pid && port == record.port => {}
        Ok(other) => return Err(keep(format!("{id} changed before the stop ({other:?})"))),
        Err(e) => return Err(keep(e)),
    }
    if let Err(e) = mise::stop_supervised(bin, &provider.state_dir, id) {
        return Err(Unconfirmed { why: format!("--stop-orphans: {e}"), recovery });
    }
    let deadline = Instant::now() + crate::process::bounded(STOP_TIMEOUT);
    loop {
        let alive = pid_alive(pid);
        let listening = record.port != 0 && accepting(record.port);
        if !alive && !listening {
            return Ok(format!(
                "stopped by --stop-orphans: supervisor id {id}, pid {pid}, port {} and start time matched the record",
                record.port
            ));
        }
        if Instant::now() >= deadline {
            let what = if alive { format!("pid {pid} is still alive") } else { format!("port {} still accepts connections", record.port) };
            return Err(Unconfirmed { why: format!("--stop-orphans asked the supervisor to stop {id}, but {what}"), recovery });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What the `gc_incomplete` hint adds when some service could be stopped with `--stop-orphans`.
pub(super) fn stop_orphans_hint(entries: &[GcEntry]) -> Option<&'static str> {
    entries
        .iter()
        .flat_map(|e| e.services.iter())
        .any(|s| s.recovery.as_ref().is_some_and(|r| r.stop.is_some()))
        .then_some(
            "or run `stack gc --stop-orphans`, which stops such a service itself only while its supervisor id, pid, \
             port and process start time all still match the record",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_time_matches_only_near_the_recorded_launch() {
        let at = 1_000_000_000u64;
        assert!(start_matches(at, at));
        assert!(start_matches(at, at - 1500), "boot-time rounding");
        assert!(start_matches(at, at + 60_000), "spawned during the start call");
        assert!(!start_matches(at, at - 5000), "started before the launch: a reused pid");
        assert!(!start_matches(at, at + 3_600_000), "started an hour later: a restart");
    }
}
