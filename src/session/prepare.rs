//! Fewer round-trips: what `exec` and `run` do before planning (`--up`, `--install`), which
//! the caller would otherwise run and retry after an error.

use super::{install, status, up, Ctx, ExecPlan, LeaseOptions, StatusReport, DEFAULT_STARTUP_TIMEOUT};
use crate::error::{Result, StackError};
use crate::provider::scratch;
use crate::state::pid_alive;
use serde_json::{json, Value};

/// What `exec` and `run` do before planning the command.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prepare {
    /// `--up`: start the stack first when there is no session, or it is stale, expired,
    /// incomplete or unhealthy.
    pub up: bool,
    /// `--ttl` with `--up`: the lease of a session `up` records.
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

/// `--up`: `status`, then `up` when it would fix what status found. The session's lease is
/// kept (a TTL, or an owner still alive) unless `ttl_secs` gives a new one. Startup gets what
/// is left of the caller's deadline, at most `up`'s default.
fn up_if_needed(ctx: &Ctx, ttl_secs: Option<u64>) -> Result<Option<Value>> {
    let report = status(ctx)?;
    let Some(reason) = needs_up(&report) else { return Ok(None) };
    let previous = report.session.as_ref().and_then(|s| s.lease.as_ref());
    let lease = match ttl_secs {
        Some(ttl) => LeaseOptions { ttl_secs: Some(ttl), owner_pid: None },
        None => LeaseOptions {
            ttl_secs: previous.and_then(|l| l.ttl_secs),
            owner_pid: previous.and_then(|l| l.owner_pid).filter(|p| pid_alive(*p)),
        },
    };
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
