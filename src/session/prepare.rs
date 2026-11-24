//! Fewer round-trips: `up --reassign-ports`, and what `exec` and `run` do before planning
//! (`--up`, `--install`), which the caller would otherwise run and retry after an error.

use super::{
    install, startup_deadline, startup_timed_out, status, up, up_within, Ctx, ExecPlan, LeaseOptions, StatusReport, UpReport,
    DEFAULT_PREPARED_TTL, DEFAULT_STARTUP_TIMEOUT,
};
#[cfg(test)]
use super::Lease;
use crate::error::{Result, StackError};
use crate::ports;
use crate::provider::scratch;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Options of `up` that are not about the lease.
#[derive(Debug, Clone, Default)]
pub struct UpOptions {
    /// On `port_conflict` with a program Stack does not own, move this checkout's conflicting
    /// (unpinned) ports and continue.
    pub reassign_ports: bool,
    /// `--no-init`: run no init task (see [`crate::init_tasks`]).
    pub no_init: bool,
    /// `--reinit <service>`: run these services' init tasks again from the first.
    pub reinit: Vec<String>,
    /// `--from-snapshot <name>`: fill data directories that do not exist yet from this
    /// snapshot before the first start (see [`crate::snapshot`]).
    pub from_snapshot: Option<String>,
    /// `--label KEY=VALUE`: labels to record on the session, added to (or replacing keys of)
    /// those it already has; none given keeps them as they are (see [`crate::labels`]).
    pub labels: std::collections::BTreeMap<String, String>,
}

/// A port `up --reassign-ports` moved.
#[derive(Debug, Clone, Serialize)]
pub struct ReassignedPort {
    pub service: String,
    pub old: u16,
    pub new: u16,
}

/// [`super::up`] with [`UpOptions`]: all within `timeout`.
pub fn up_with(ctx: &Ctx, lease: LeaseOptions, timeout: Duration, opts: UpOptions) -> Result<UpReport> {
    let _deadline = crate::process::deadline_scope(Some(startup_deadline(timeout)?));
    up_reassigning(ctx, lease, opts).map_err(|e| startup_timed_out(e, "up", timeout))
}

/// `up`, and with `reassign_ports`, after a `port_conflict` at the `ports` step that names no
/// pinned port, new reservations for the conflicting services and `up` once more.
fn up_reassigning(ctx: &Ctx, lease: LeaseOptions, opts: UpOptions) -> Result<UpReport> {
    let first = up_within(ctx, lease.clone(), &opts);
    let error = match first {
        Err(e) if opts.reassign_ports && e.code == "port_conflict" && failed_step(&e) == Some("ports") => e,
        other => return other,
    };
    // Only ports Stack assigned can move; a pinned one keeps the error and its hint.
    let conflicts: Vec<&Value> = error.details.iter().filter(|d| d.get("service").is_some() && d.get("port").is_some()).collect();
    if conflicts.is_empty() || conflicts.iter().any(|c| c["pinned"] == true) {
        return Err(error);
    }
    let services: Vec<String> = conflicts.iter().filter_map(|c| c["service"].as_str().map(String::from)).collect();
    let moved = ports::reassign(&ctx.state, &ctx.root, &services)?;
    let mut report = up_within(ctx, lease, &opts)?;
    report.reassigned_ports = moved.into_iter().map(|(service, old, new)| ReassignedPort { service, old, new }).collect();
    Ok(report)
}

/// The step an `up` failure's progress record names as failed.
fn failed_step(error: &StackError) -> Option<&str> {
    error
        .details
        .iter()
        .rev()
        .find_map(|d| d.get("steps"))
        .and_then(|steps| steps.as_array()?.last()?.get("step")?.as_str())
}

/// What `exec` and `run` do before planning the command.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prepare {
    /// `--up`: start the stack first when there is no session, or it is stale, expired,
    /// incomplete or unhealthy.
    pub up: bool,
    /// `--ttl` with `--up`: the lease of a session `up` records. Without it a session that has
    /// no lease gets `[session] default_ttl`, else 30 minutes.
    pub ttl_secs: Option<u64>,
    /// `--install`: install pinned tools that are missing instead of failing.
    pub install: bool,
}

/// Run what `prepare` asks, then `plan`. What ran is attached to the plan as `prepared`.
pub fn plan_prepared(ctx: &Ctx, prepare: Prepare, plan: impl FnOnce() -> Result<ExecPlan>) -> Result<ExecPlan> {
    let mut prepared = Vec::new();
    if prepare.up {
        prepared.extend(up_if_needed(ctx, prepare.ttl_secs)?);
    }
    if prepare.install {
        prepared.extend(install_if_missing(ctx)?);
    }
    let mut plan = plan()?;
    plan.prepared = prepared;
    Ok(plan)
}

/// Why `up` should run before a command, judged from a live `status`, if it should.
pub fn needs_up(report: &StatusReport) -> Option<String> {
    let Some(session) = &report.session else { return Some("no session for this checkout".into()) };
    if session.launching {
        return Some("the last start did not finish verifying".into());
    }
    if report.stale {
        return Some("the configuration changed since the services started".into());
    }
    if let Some(why) = report.lease.expired_reason.as_ref() {
        return Some(why.clone());
    }
    report
        .checks
        .iter()
        .find(|c| !c.ready)
        .map(|c| format!("{} is not verified: {}", c.service, c.reason.as_deref().unwrap_or("unknown")))
}

/// The lease `--up` asks `up` for: `ttl_secs` when given, else preserve the session's current
/// lease read under the startup lock (a TTL, or an owner still alive). A session that has
/// neither gets the project's `[session] default_ttl`, else [`DEFAULT_PREPARED_TTL`], never a
/// lease that lives until `stack down`.
fn prepared_lease(ttl_secs: Option<u64>) -> LeaseOptions {
    match ttl_secs {
        Some(ttl) => LeaseOptions { ttl_secs: Some(ttl), owner_pid: None, fallback_ttl_secs: None, keep_existing_lease: false },
        None => LeaseOptions {
            ttl_secs: None,
            owner_pid: None,
            fallback_ttl_secs: Some(DEFAULT_PREPARED_TTL),
            keep_existing_lease: true,
        },
    }
}

/// `--up`: `status`, then `up` when it would fix what status found. The session's lease is
/// kept (a TTL, or an owner still alive, read under `up`'s project lock so a concurrent
/// `renew` or `up` is never overwritten) unless `ttl_secs` gives a new one; a session without
/// one gets a default TTL (see [`prepared_lease`]). Startup gets what is left of the caller's
/// deadline, at most `up`'s default.
fn up_if_needed(ctx: &Ctx, ttl_secs: Option<u64>) -> Result<Option<Value>> {
    let report = status(ctx)?;
    let Some(reason) = needs_up(&report) else { return Ok(None) };
    let lease = prepared_lease(ttl_secs);
    let timeout = crate::process::remaining().map_or(DEFAULT_STARTUP_TIMEOUT, |r| r.min(DEFAULT_STARTUP_TIMEOUT));
    if timeout.is_zero() {
        return Err(StackError::new("timed_out", "no time is left to start the stack"));
    }
    let started = up(ctx, lease, timeout)?;
    Ok(Some(json!({
        "step": "up",
        "reason": reason,
        "session": started.session.id,
        "lease": crate::lease::LeaseSummary::of(started.session.lease.as_ref()),
        "warnings": started.warnings,
    })))
}

/// `--install`: `stack install` when a pinned release is not installed, else nothing.
fn install_if_missing(ctx: &Ctx) -> Result<Option<Value>> {
    let report = ctx.compile(false)?;
    let pins = report.lock.as_ref().map(scratch::pins).unwrap_or_default();
    let missing = scratch::not_installed(&ctx.cache, &pins)?;
    if missing.is_empty() {
        return Ok(None);
    }
    let names: Vec<String> = missing.iter().map(|p| format!("{}@{}", p.tool, p.spec.version)).collect();
    let installed = install(ctx)?;
    Ok(Some(json!({ "step": "install", "missing": names, "warnings": installed.warnings })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_up_keeps_a_sessions_lease_and_gives_one_without_a_default_ttl() {
        let lease = |ttl_secs, owner_pid| Lease { ttl_secs, owner_pid, renewed_at: 0 };
        let resolved = |ttl: Option<u64>, previous: Option<&Lease>, project: Option<u64>| {
            prepared_lease(ttl).resolve_with_previous(project, previous, |pid| pid == 7)
        };
        // No session, or one without a lease: the project's default, else 30 minutes, never a
        // session that outlives the call forever.
        assert_eq!(resolved(None, None, None), (Some(DEFAULT_PREPARED_TTL), None));
        assert_eq!(DEFAULT_PREPARED_TTL, 1800);
        assert_eq!(resolved(None, None, Some(600)), (Some(600), None));
        // An owner that exited leaves no lease to keep.
        assert_eq!(resolved(None, Some(&lease(None, Some(8))), None), (Some(1800), None));
        // A session's lease is kept, over the project's default.
        assert_eq!(resolved(None, Some(&lease(Some(300), None)), Some(600)), (Some(300), None));
        assert_eq!(resolved(None, Some(&lease(None, Some(7))), Some(600)), (None, Some(7)));
        assert_eq!(resolved(None, Some(&lease(Some(300), Some(7))), None), (Some(300), Some(7)));
        // --ttl replaces whatever the session had.
        assert_eq!(resolved(Some(90), Some(&lease(Some(300), Some(7))), Some(600)), (Some(90), None));
        assert_eq!(resolved(Some(90), None, None), (Some(90), None));
    }
}
