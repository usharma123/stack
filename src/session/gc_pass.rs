//! How a GC pass chooses what to lock, so that one checkout's `up` never waits on, or fails
//! because of, another checkout.

use super::{gc_locked, project_gone, GcEntry, Session, Steps};
use crate::error::Result;
use crate::state::{now, project_lock, project_try_lock, read_json};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// How a GC pass treats other projects.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcOptions {
    /// The pass `up` runs first: never wait for another project's lock (a busy candidate is
    /// skipped and reported), and never fail (problems become warnings).
    pub background: bool,
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
}

#[derive(Debug, Serialize)]
pub struct GcSkipped {
    pub project: PathBuf,
    pub reason: String,
    /// Why it was not reclaimed now: `busy` (another stack command holds its lock).
    pub skipped: &'static str,
}

/// Why `session` may be reclaimed, judged from what it records without taking its lock. A
/// session that is not a candidate is never locked; one that is gets checked again under
/// the lock, since another command may renew or replace it meanwhile.
pub(super) fn candidate(session: &Session, now: u64) -> Option<String> {
    project_gone(session)
        .map(String::from)
        .or_else(|| session.lease.as_ref().and_then(|l| l.expired(now)))
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
            let indexed: Session = read_json(&path)?;
            let Some(reason) = candidate(&indexed, now()) else { return Ok(()) };
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
            if let Some(entry) = gc_locked(state, &path)? {
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
    let pass = gc_with(state, &GcOptions { background: true }).unwrap_or_else(|e| GcPass {
        warnings: vec![e.to_string()],
        ..GcPass::default()
    });
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
