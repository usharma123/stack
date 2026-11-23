//! Sessions: one running instance of a project on this machine.
//!
//! The session record says what stack started; it is never treated as proof of what is running.
//! Every `exec` and `status` re-checks each service against the supervisor and, where the service
//! type allows, confirms over the app's own connection settings that it reaches *this* instance.
//! Connection variables for a service that fails the check are withheld from commands.

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use crate::compose::{Composed, Entry};
use crate::manifest::{Service, Task};
use crate::artifacts;
use crate::ports;
use crate::project::{self, Options, Report};
use crate::provider::mise;
use crate::supervisor::{self, DaemonStatus};
use crate::provider::scratch;
pub use crate::provider::task_config::TaskConfig;
use crate::secrets::{self, Grant};
use crate::source::Mode;
use crate::state::{now, pid_alive, project_key, project_lock, read_json, write_json, SharedLock};
use crate::timing::Timings;
use crate::lease::LeaseSummary;

mod gc_pass;
mod prepare;
use crate::commands;
pub use crate::commands::RunningCommand;
pub use prepare::{needs_up, plan_prepared, up_with, Prepare, ReassignedPort, UpOptions};
pub(crate) use gc_pass::readable_session;
pub use gc_pass::{gc_plan, gc_with, DeletedPlan, GcDryRun, GcOptions, GcPass, GcPlanned, GcSkipped, PlannedData, PlannedPort, PlannedService};
use crate::tool::ToolSpec;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `up` and `restart` may take in all, unless the caller says otherwise.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(600);
/// Time allowed past an expired deadline to record what a cut-short start launched.
const RECORD_GRACE: Duration = Duration::from_secs(10);
/// How long a finished command waits for a project lock another command holds, with or
/// without a deadline, before leaving a completion record for the next lifecycle command.
const RELEASE_GRACE: Duration = Duration::from_secs(1);

pub struct Ctx {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
}

impl Ctx {
    fn options(&self, write: bool) -> Options {
        Options {
            root: self.root.clone(),
            mode: Mode::Frozen,
            write,
            cache: self.cache.clone(),
            state: self.state.clone(),
            reassign_ports: false,
            resolver: None,
            locker: None,
        }
    }

    fn compile(&self, write: bool) -> Result<Report> {
        project::compile_locked(&self.options(write))
    }

    /// The locked, writing compile of `install` and `up`. mise must be new enough for the
    /// configuration and the embedded lock before anything is published: port and identity
    /// reservations, the generated config. A provider failure belongs to the `install` step,
    /// whose requirement it is; anything else to `compile`.
    fn compile_to_install(&self) -> std::result::Result<Report, (&'static str, StackError)> {
        let provider_failed = std::cell::Cell::new(false);
        project::compile_checked(&self.options(true), &|stack, lock| {
            let checked = mise::require_in(&self.state, &self.root, &project::requirements(stack, Some(lock))).map(drop);
            provider_failed.set(checked.is_err());
            checked
        })
        .map_err(|e| (if provider_failed.get() { "install" } else { "compile" }, e))
    }

    fn session_file(&self) -> PathBuf {
        self.root.join(".stack").join("session.json")
    }

    fn index_file(&self) -> PathBuf {
        index_path(&self.state, &self.root)
    }
}

pub(crate) fn index_path(state: &Path, root: &Path) -> PathBuf {
    state
        .join("sessions")
        .join(format!("{}.json", project_key(root)))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project: PathBuf,
    /// Hash of stack.lock at start. A different lock means the running services are stale.
    pub lock_digest: String,
    /// Complete compiled configuration and ports at launch, including project overrides.
    #[serde(default)]
    pub config_digest: String,
    /// Coordinators currently executing against this generation. The next lifecycle command
    /// forgets those that have exited.
    #[serde(default)]
    pub active_executions: IndexMap<String, u32>,
    pub started_at: u64,
    /// Written before services start, so a failed or interrupted `up` still records what it may
    /// have launched. Cleared once every service verifies.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launching: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    pub services: IndexMap<String, ServiceRecord>,
    /// How to reach the supervisor without the project directory, so services can still be
    /// stopped after the project is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderRecord>,
    /// Device and inode of the project directory at launch. A directory recreated at the same
    /// path is a different project and never inherits this session's authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir_id: Option<(u64, u64)>,
    /// Creation time distinguishes directories when the filesystem reuses an inode.
    /// Older records and filesystems without birth times retain device/inode checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir_created: Option<std::time::SystemTime>,
    /// `stack up --label KEY=VALUE`: what the caller said this session is for, which `ps`,
    /// `status`, `logs` and `down` select checkouts by (see [`crate::labels`]). Kept across
    /// `up`s of the same checkout; a later `up --label` adds or replaces keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// The supervisor that runs a session's services: Stack's own (`supervisor: "stack"`, its
/// directory in `state_dir`), or, for a session Stack 0.1.23 or earlier started, Pitchfork (its
/// binary and state directory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// The Pitchfork binary mise used for this project (sessions of Stack 0.1.23 and earlier);
    /// empty for Stack's own supervisor (older releases require the field).
    #[serde(default, with = "crate::state::required_path")]
    pub pitchfork: Option<PathBuf>,
    /// Stack's supervisor directory, or Pitchfork's effective state directory.
    pub state_dir: PathBuf,
    /// `stack` for Stack's own supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
}

impl ProviderRecord {
    pub fn native(dir: PathBuf) -> Self {
        Self { pitchfork: None, state_dir: dir, supervisor: Some("stack".into()) }
    }

    /// The Pitchfork binary, for a session an older Stack started under Pitchfork.
    pub fn legacy(&self) -> Option<&Path> {
        self.pitchfork.as_deref()
    }
}

/// The session's services run under Pitchfork, started by Stack 0.1.23 or earlier.
fn legacy_provider(session: Option<&Session>) -> Option<&ProviderRecord> {
    session.and_then(|s| s.provider.as_ref()).filter(|p| p.legacy().is_some())
}

/// When a session may be reclaimed. With neither field set it lives until `stack down`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// A long-lived runner process; the session expires when it exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    pub renewed_at: u64,
}

impl Lease {
    pub fn expired(&self, now: u64) -> Option<String> {
        if let Some(ttl) = self.ttl_secs {
            let idle = now.saturating_sub(self.renewed_at);
            if idle > ttl {
                return Some(format!("lease expired: idle {idle}s > ttl {ttl}s"));
            }
        }
        match self.owner_pid {
            Some(pid) if !pid_alive(pid) => Some(format!("owner process {pid} exited")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    pub identity: Identity,
    pub verified_at: u64,
    /// When stack last started this process, in Unix milliseconds (taken before the
    /// supervisor's start call). Records from before 0.1.18, and processes they describe, lack
    /// it: then nothing is known about the files it loaded or where its output begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// The supervisor's qualified id for this daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// How strongly a service was verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// Connected using the app's own settings and confirmed the server's data directory.
    Instance,
    /// Supervisor reports it running and the port accepts connections. Identity not checked.
    Liveness,
}

/// A live check of one service. Never cached: an observation at `checked_at`.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub service: String,
    pub ready: bool,
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `reason` as a stable code an agent can branch on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<ReasonCode>,
    /// Connection variables poisoned or removed for commands because the service is not verified.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub withheld: Vec<String>,
    /// Files under the service's `watch` paths modified after it started: the process may be
    /// running older code. Reported, not a verification failure.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_since_start: Vec<String>,
    /// The `watch` paths hold more entries than one check examines; changes may be missed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub watch_incomplete: bool,
    pub checked_at: u64,
    #[serde(skip)]
    data_dir: Option<String>,
    #[serde(skip)]
    provider_id: Option<String>,
}

/// Why a service check failed, as a stable code beside the free-text `reason`. The values and
/// the action each calls for are listed in docs/user/commands.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// Nothing in this checkout launched it (`up`).
    NotLaunched,
    /// The last `up` did not finish verifying this launch (`up`).
    LaunchIncomplete,
    /// The compiled configuration changed since `up` (`up`).
    ConfigChanged,
    /// The process differs from the one `up` verified, or is gone (`up`).
    ProcessChanged,
    /// No port assigned (`compile`).
    NoPort,
    /// No instance token assigned for its identity probe (`compile`).
    NoToken,
    /// The supervisor does not know it (`up`).
    NotStarted,
    /// The supervisor reports it stopped, failed or still starting (`logs`, then `restart`).
    NotRunning,
    /// Its process is gone (`restart`).
    ProcessDead,
    /// It runs on a port other than this checkout's (`down`, then `up`).
    WrongPort,
    /// Its port does not accept connections (`logs`, then `restart`).
    PortClosed,
    /// A connection variable it needs is not set (`compile`, then `up`).
    EnvMissing,
    /// The identity check could not run or gave no answer (`logs`).
    ProbeFailed,
    /// The app's settings reach a different instance (`down`, then `up`).
    WrongInstance,
}

// ---- verification --------------------------------------------------------------------------

/// Services verified at once. A check mostly waits on a socket or a client process, so a few
/// threads hide that latency; the bound keeps a large stack from spawning every client at once.
const VERIFY_CONCURRENCY: usize = 8;

/// Check every service now. Services are independent, so they are checked concurrently; the
/// result keeps manifest order.
fn verify_all(
    root: &Path,
    report: &Report,
    env: &IndexMap<String, String>,
    statuses: &[DaemonStatus],
    timings: &Timings,
) -> Vec<Check> {
    verify_some(root, report, env, statuses, timings, None)
}

/// [`verify_all`] for `only` those services (all when `None`), in manifest order.
fn verify_some(
    root: &Path,
    report: &Report,
    env: &IndexMap<String, String>,
    statuses: &[DaemonStatus],
    timings: &Timings,
    only: Option<&[String]>,
) -> Vec<Check> {
    let inherited = inherited_env();
    let check = |&(name, service): &(&String, &Service)| {
        let port = report.ports.get(name).copied();
        let status = statuses.iter().find(|d| d.name == *name);
        let mut check = Check {
            service: name.clone(),
            ready: false,
            port,
            pid: status.and_then(|s| s.pid),
            identity: None,
            reason: None,
            reason_code: None,
            withheld: Vec::new(),
            changed_since_start: Vec::new(),
            watch_incomplete: false,
            checked_at: now(),
            data_dir: status.and_then(|s| s.data_dir.clone()),
            provider_id: status.and_then(|s| s.id.clone()),
        };
        let token = report.identities.get(name).map(String::as_str);
        let started = Instant::now();
        let verified = verify_one(root, service, port, status, env, token);
        if timings.enabled() {
            timings.record(&format!("verify.{name}"), started.elapsed());
        }
        match verified {
            Ok(identity) => {
                check.ready = true;
                check.identity = Some(identity);
            }
            Err((code, reason)) => {
                check.reason = Some(reason);
                check.reason_code = Some(code);
                check.withheld = binding_vars(name, service, port, env, &inherited);
            }
        }
        check
    };
    let services: Vec<(&String, &Service)> = report
        .stack
        .services
        .iter()
        .filter(|(name, _)| only.map_or(true, |only| only.contains(name)))
        .map(|(name, entry)| (name, &entry.value))
        .collect();
    concurrently(&services, VERIFY_CONCURRENCY, check)
}

/// `items.iter().map(f).collect()`, run on up to `limit` scoped threads. Results keep the order
/// of `items`; a panic in `f` is propagated to the caller.
fn concurrently<T: Sync, R: Send>(items: &[T], limit: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = items.len().min(limit);
    if workers <= 1 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let bound = crate::process::inherit();
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let _deadline = bound.enter();
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break mine };
                        mine.push((i, f(item)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| {
                h.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_unstable_by_key(|&(i, _)| i);
    done.into_iter().map(|(_, r)| r).collect()
}

fn verify_one(
    root: &Path,
    service: &Service,
    port: Option<u16>,
    status: Option<&DaemonStatus>,
    env: &IndexMap<String, String>,
    token: Option<&str>,
) -> std::result::Result<Identity, (ReasonCode, String)> {
    use ReasonCode as R;
    let port = port.ok_or((R::NoPort, "no port assigned; run `stack compile`".to_string()))?;
    let status = status.ok_or((R::NotStarted, "not started".to_string()))?;
    if status.status != "running" {
        return Err((R::NotRunning, format!("supervisor reports '{}'", status.status)));
    }
    if let Some(pid) = status.pid {
        if !pid_alive(pid) {
            return Err((R::ProcessDead, format!("process {pid} is not alive")));
        }
    }
    if let Some(actual) = status.port {
        if actual != port {
            return Err((R::WrongPort, format!("running on port {actual}, but this checkout is assigned {port}; run `stack down && stack up`")));
        }
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|_| (R::PortClosed, format!("port {port} is not accepting connections")))?;

    let (var, bin) = match service.preset.as_deref() {
        Some("postgres") => ("DATABASE_URL", "psql"),
        Some("redis") => ("REDIS_URL", "redis-cli"),
        _ => {
            return match &service.identity {
                Some(probe) => {
                    let token = token.ok_or((R::NoToken, "no instance token assigned; run `stack compile`".to_string()))?;
                    run_identity_probe(root, probe, env, token).map(|()| Identity::Instance).map_err(|why| {
                        // The one answer that is not a failure to probe.
                        let code = if why.starts_with("identity probe reached a different instance") { R::WrongInstance } else { R::ProbeFailed };
                        (code, why)
                    })
                }
                None => Ok(Identity::Liveness),
            }
        }
    };
    let url = env.get(var).ok_or_else(|| (R::EnvMissing, format!("{var} is not set")))?;
    let expected = status
        .data_dir
        .as_deref()
        .ok_or((R::ProbeFailed, "supervisor did not report a data directory".to_string()))?;
    let query = "select current_setting('data_directory')";
    let args: Vec<&str> = match bin {
        // `-X`: no ~/.psqlrc, whose settings (`\timing`, say) would change what it prints.
        "psql" => vec!["-X", url, "-Atc", query],
        _ => vec!["-u", url, "--no-auth-warning", "config", "get", "dir"],
    };
    // Asked over the wire when that is exactly the client's question; the client otherwise.
    let asked = match bin {
        "psql" => crate::wire::postgres(url, env, query),
        _ => crate::wire::redis_dir(url, env),
    };
    let out = match asked {
        crate::wire::Asked::Answer(out) => out,
        crate::wire::Asked::Fallback(_) => run_probe(env, bin, &args).map_err(|why| (R::ProbeFailed, why))?,
        crate::wire::Asked::Failed(why) => return Err((R::ProbeFailed, why)),
    };
    let reported = out
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    if same_path(reported, expected) {
        Ok(Identity::Instance)
    } else {
        Err((R::WrongInstance, format!(
            "{var} reaches a different server (data dir '{reported}', expected '{expected}')"
        )))
    }
}

/// Bytes of probe output kept; an identity is one short line, so more is malformed.
const IDENTITY_OUTPUT_LIMIT: usize = 4096;

/// Run a bundle's identity probe and require it to print exactly this instance's token.
/// The probe gets the app's environment without any instance token, so it can only learn the
/// token from the service. Its process group is killed at the deadline or on completion, and
/// its output is bounded.
fn run_identity_probe(
    root: &Path,
    probe: &crate::manifest::IdentityProbe,
    env: &IndexMap<String, String>,
    token: &str,
) -> std::result::Result<(), String> {
    let timeout = Duration::from_secs(probe.timeout_secs());
    let mut command = Command::new("sh");
    command.args(["-c", &probe.command]).current_dir(root);
    mise::configure_command(&mut command, root);
    command.envs(env.iter().filter(|(k, _)| !k.starts_with("STACK_IDENTITY_")));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("STACK_IDENTITY_")) {
            command.env_remove(&key);
        }
    }
    let out = crate::process::capture(&mut command, timeout, IDENTITY_OUTPUT_LIMIT)
        .map_err(|e| format!("cannot run identity probe: {e}"))?;
    if out.timed_out {
        return Err(format!("identity probe did not answer within {}s", timeout.as_secs()));
    }
    if out.exit_code != Some(0) {
        let err = out.stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
        let code = out.exit_code.map_or("a signal".to_string(), |c| c.to_string());
        return Err(format!("identity probe exited with {code}: {}", truncate(err, 200)));
    }
    if out.stdout_truncated {
        return Err(format!("identity probe printed more than {IDENTITY_OUTPUT_LIMIT} bytes"));
    }
    match out.stdout.trim() {
        "" => Err("identity probe printed no identity".into()),
        reported if reported == token => Ok(()),
        reported => Err(format!(
            "identity probe reached a different instance (reported '{}')",
            truncate(reported, 80)
        )),
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    !a.is_empty() && canon(a) == canon(b)
}

/// Bytes of client output kept. The probes print one short line; more is not an answer.
const PROBE_OUTPUT_LIMIT: usize = 16 * 1024;

/// Run a client binary from the stack's PATH with a hard deadline. Like identity probes, it runs
/// in its own process group, which is killed at the deadline or on completion, and its output is
/// bounded.
fn run_probe(
    env: &IndexMap<String, String>,
    bin: &str,
    args: &[&str],
) -> std::result::Result<String, String> {
    let path = which_in(env.get("PATH").map(String::as_str), bin)
        .ok_or_else(|| format!("{bin} not found on the stack's PATH"))?;
    let mut command = Command::new(path);
    command.args(args).envs(env).env("PGCONNECT_TIMEOUT", "3");
    let out = crate::process::capture(&mut command, PROBE_TIMEOUT, PROBE_OUTPUT_LIMIT)
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.timed_out {
        return Err(format!(
            "{bin} did not answer within {}s",
            PROBE_TIMEOUT.as_secs()
        ));
    }
    if out.exit_code != Some(0) {
        return Err(format!(
            "{bin} failed: {}",
            out.stderr.lines().next().unwrap_or("").trim()
        ));
    }
    if out.stdout_truncated {
        return Err(format!(
            "{bin} printed more than {PROBE_OUTPUT_LIMIT} bytes"
        ));
    }
    Ok(out.stdout)
}

pub fn which_in(path: Option<&str>, bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        return Some(PathBuf::from(bin));
    }
    std::env::split_paths(path?)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Variables that point a command at this service: its preset's and name's variables, whether
/// set by the provider or inherited from the caller, and provider values naming its port.
fn binding_vars(
    name: &str,
    service: &Service,
    port: Option<u16>,
    env: &IndexMap<String, String>,
    inherited: &IndexMap<String, String>,
) -> Vec<String> {
    let mut vars: Vec<String> = match service.preset.as_deref() {
        Some("postgres") => [
            "DATABASE_URL",
            "PGHOST",
            "PGHOSTADDR",
            "PGSERVICE",
            "PGPORT",
            "PGUSER",
            "PGDATABASE",
            "PGPASSWORD",
        ]
        .map(String::from)
        .to_vec(),
        Some("redis") => vec!["REDIS_URL".into()],
        _ => Vec::new(),
    };
    let folded = name.to_uppercase().replace('-', "_");
    vars.push(format!("{folded}_HOST"));
    vars.push(format!("{folded}_PORT"));
    vars.push(format!("{folded}_URL"));
    if let Some(port) = port {
        let (bare, colon) = (port.to_string(), format!(":{port}"));
        vars.extend(
            env.iter()
                .filter(|(_, v)| **v == bare || v.contains(&colon))
                .map(|(k, _)| k.clone()),
        );
    }
    let mut out: Vec<String> = Vec::new();
    for v in vars {
        // libpq falls back to its default host when PGHOST is unset, so it is always poisoned.
        let set = env.contains_key(&v) || inherited.contains_key(&v);
        if (set || (v == "PGHOST" && service.preset.as_deref() == Some("postgres")))
            && !out.contains(&v)
        {
            out.push(v);
        }
    }
    out
}

/// The caller's environment, which commands inherit beneath the provider's. Used only to find
/// and poison connection variables; commands inherit the raw values of everything else. Unix
/// allows any bytes, so this must not panic like `std::env::vars`: a key that is not Unicode
/// cannot name a connection variable, and a value that is not is read lossily so it is still
/// withheld rather than passed through.
fn inherited_env() -> IndexMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.to_string_lossy().into_owned())))
        .collect()
}

// ---- supervision ---------------------------------------------------------------------------

/// Whether a listed service may be running: a process with its recorded start time runs
/// (whatever the status says: `orphaned` and `stopping` ones run too), or the supervisor says
/// it is running or starting.
pub(crate) fn may_run(d: &DaemonStatus) -> bool {
    d.pid.is_some() || matches!(d.status.as_str(), "running" | "starting" | "waiting" | "stopping" | "orphaned")
}

/// The directory of this checkout's supervisor (see [`crate::supervisor`]).
fn supervisor_dir(ctx: &Ctx) -> PathBuf {
    supervisor::dir(&ctx.state, &ctx.root)
}

/// Each configured service with its port and, for a preset that keeps data, its data directory.
fn configured(ctx: &Ctx, report: &Report) -> Vec<(String, u16, Option<PathBuf>)> {
    report
        .stack
        .services
        .iter()
        .filter_map(|(name, e)| {
            let port = *report.ports.get(name)?;
            let keeps = e.value.preset.as_deref().and_then(supervisor::preset).is_some_and(|p| p.keeps_data);
            Some((name.clone(), port, keeps.then(|| data_dir_for(ctx, name))))
        })
        .collect()
}

/// Where `service`'s data is: the directory its supervisor last ran it with, else the one `up`
/// recorded for this path (`data.json`), else the one a session of an older Stack recorded,
/// else what mise's preset kept for this path, else a new one (see
/// [`supervisor::new_data_dir`]). Existing data is never left behind for an empty directory.
fn data_dir_for(ctx: &Ctx, service: &str) -> PathBuf {
    let dir = supervisor_dir(ctx);
    if let Some(d) = supervisor::state_lossy(&dir).services.get(service).and_then(|i| i.data_dir.clone()) {
        return d;
    }
    if let Some(d) = supervisor::read_spec(&dir).ok().flatten().and_then(|s| s.services.get(service).and_then(|i| i.data_dir.clone())) {
        return d;
    }
    if let Ok(entries) = crate::datadirs::entries(&ctx.state) {
        if let Some(e) = entries.iter().find(|e| e.project == ctx.root && e.service == service) {
            return e.dir.clone();
        }
    }
    if let Some(d) = read_json::<Session>(&ctx.index_file()).ok().and_then(|s| s.services.get(service).and_then(|r| r.data_dir.clone())) {
        return PathBuf::from(d);
    }
    supervisor::legacy_data_dir(&ctx.root, service).unwrap_or_else(|| supervisor::new_data_dir(&ctx.state, &ctx.root, service))
}

/// What Stack's supervisor runs for this checkout, and every other configured service as
/// `available`. Files only: no process is started.
fn listing(ctx: &Ctx, report: &Report) -> Vec<DaemonStatus> {
    supervisor::daemons(&supervisor_dir(ctx), &configured(ctx, report))
}

/// [`listing`], or what Pitchfork reports for a session an older Stack started under it.
fn observed(ctx: &Ctx, report: &Report, session: Option<&Session>) -> Vec<DaemonStatus> {
    match (session, legacy_provider(session)) {
        (Some(session), Some(provider)) => legacy_statuses(session, provider),
        _ => listing(ctx, report),
    }
}

/// `start_failed` once one of `names` exited on its own since it was started: no wait for it
/// to verify can end otherwise. Carries the last lines it wrote.
fn exited_on_start(ctx: &Ctx, statuses: &[DaemonStatus], names: &[String]) -> Result<()> {
    let state = supervisor::state_lossy(&supervisor_dir(ctx));
    let failed: Vec<&DaemonStatus> = statuses.iter().filter(|d| d.status == "failed" && names.contains(&d.name)).collect();
    let Some(first) = failed.first() else { return Ok(()) };
    let details: Vec<Value> = failed
        .iter()
        .map(|d| {
            let lines = supervisor::logs(&supervisor_dir(ctx), &d.name, 12, None).map(|(l, _)| l).unwrap_or_default();
            let exit = state.services.get(&d.name).and_then(|i| i.exit.clone());
            json!({ "service": d.name, "exit": exit, "output": lines.join("\n") })
        })
        .collect();
    let how = state.services.get(&first.name).and_then(|i| i.exit.clone()).unwrap_or_else(|| "it ended".into());
    Err(StackError::new("start_failed", format!("{} exited while starting ({how})", first.name))
        .hint(format!("`stack logs {}` shows why; fix it and run `stack up` again", first.name))
        .details(details))
}

/// How the supervisor runs each service of `report`, with `env` (the provider's environment for
/// this checkout) over what this command inherited, the preset's own variables and the
/// checkout's token and the service's `$PORT` (a project's `[env]` value wins, as under mise).
fn service_specs(ctx: &Ctx, report: &Report, env: &IndexMap<String, String>) -> Result<IndexMap<String, supervisor::ServiceSpec>> {
    let token = crate::checkout::token(&ctx.root);
    let dropped = mise::inherited_config_keys();
    let base: IndexMap<String, String> = inherited_env()
        .into_iter()
        .filter(|(k, _)| !k.starts_with("STACK_") && !dropped.contains(k))
        .collect();
    let mut specs = IndexMap::new();
    for (name, entry) in &report.stack.services {
        let service = &entry.value;
        let Some(port) = report.ports.get(name).copied() else { continue };
        let mut service_env = base.clone();
        for &(key, value) in mise::preset_env(service.preset.as_deref()) {
            service_env.insert(key.into(), value.into());
        }
        if let Some(token) = &token {
            service_env.insert(crate::checkout::TOKEN_VAR.into(), token.clone());
        }
        service_env.insert("PORT".into(), port.to_string());
        service_env.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        // Never a command's mark, whatever the provider's environment holds: a service carrying
        // one would be taken for a command and never stopped (see `crate::commands`).
        service_env.shift_remove(crate::checkout::COMMAND_VAR);
        let declared = match (&service.ready_cmd, service.ready_port) {
            (Some(command), _) => Some(supervisor::Ready::Command { command: command.clone() }),
            (None, Some(port)) => Some(supervisor::Ready::Port { port }),
            _ => None,
        };
        let (argv, ready, stop_signal, data_dir) = match service.preset.as_deref() {
            Some(name_of) => {
                let preset = supervisor::preset(name_of).ok_or_else(|| {
                    StackError::new("start_failed", format!("services.{name} uses preset {name_of:?}, which Stack does not run"))
                        .hint("use one of postgres, redis, cockroachdb, nats, spicedb, or give the service a `run` command")
                })?;
                let dir = data_dir_for(ctx, name);
                let pinned = report.versions.iter().find(|v| v.kind == "service" && v.name == *name).and_then(|v| v.resolved.as_deref());
                // Its own pin's server first on its PATH, whatever other releases of the preset
                // the stack installs.
                if let (Some(tool), Some(pinned), Some(path)) = (mise::preset_tool(preset.name), pinned, service_env.get("PATH").cloned()) {
                    service_env.insert("PATH".into(), supervisor::pinned_path(&path, tool, pinned));
                }
                let secondary: supervisor::Secondary = supervisor::secondary_roles(preset.name)
                    .iter()
                    .filter_map(|role| Some((role.to_string(), *report.secondary_ports.get(&format!("{name}.{role}"))?)))
                    .collect();
                let ready = declared.unwrap_or_else(|| supervisor::preset_ready(preset.name, port, &secondary));
                (supervisor::preset_argv(preset.name, &dir, port, pinned, &secondary, Some(&crate::pgtemplate::root(&ctx.state)))?, ready, preset.stop_signal, Some(dir))
            }
            None => {
                let run = service.run.clone().ok_or_else(|| StackError::new("start_failed", format!("services.{name} has neither a preset nor a run command")))?;
                (vec!["/bin/sh".into(), "-c".into(), run], declared.unwrap_or(supervisor::Ready::Spawned), libc::SIGTERM, None)
            }
        };
        specs.insert(
            name.clone(),
            supervisor::ServiceSpec { argv, env: service_env, cwd: ctx.root.clone(), port, ready, stop_signal, data_dir, retries: 0, want: false, start_seq: 0 },
        );
    }
    Ok(specs)
}

/// Whether the service a data record names is confirmed not running by the supervisor recorded
/// with it: Stack's own (its state), or Pitchfork for data an older Stack recorded.
pub(crate) fn service_stopped(supervisor: &crate::datadirs::Supervisor) -> std::result::Result<(), String> {
    match &supervisor.pitchfork {
        None => {
            let state = supervisor::state_lossy(&supervisor.state_dir);
            if let Some((i, p)) = state.services.get(&supervisor.id).and_then(|i| i.live().map(|p| (i, p))) {
                return Err(format!("Stack's supervisor reports {} {} (pid {})", supervisor.id, i.status, p.pid));
            }
            let unresolved = supervisor::unresolved(&supervisor.state_dir);
            if !unresolved.is_empty() {
                return Err(format!("processes of the checkout's services may still run and could not be confirmed stopped: {}", Value::Array(unresolved)));
            }
            Ok(())
        }
        Some(bin) => match mise::supervised(bin, &supervisor.state_dir, &supervisor.id) {
            Ok(mise::Supervised::NotFound) => Ok(()),
            Ok(mise::Supervised::Found { status, pid, .. }) if status == "stopped" && !pid.is_some_and(pid_alive) => Ok(()),
            Ok(mise::Supervised::Found { status, .. }) => Err(format!("the supervisor reports {} as {status:?}", supervisor.id)),
            Err(why) => Err(format!("cannot confirm its service stopped ({why})")),
        },
    }
}

// ---- lifecycle -----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
struct Steps(Vec<Value>);

impl Steps {
    fn ok(&mut self, step: &str, detail: Value) {
        crate::progress::notify(crate::progress::Event { step, phase: crate::progress::Phase::Ok, code: None, detail: Some(&detail) });
        self.0
            .push(json!({ "step": step, "status": "ok", "detail": detail }));
    }

    /// A step that completed with warnings: the command still succeeds.
    fn warn(&mut self, step: &str, detail: Value) {
        crate::progress::notify(crate::progress::Event { step, phase: crate::progress::Phase::Warning, code: None, detail: Some(&detail) });
        self.0
            .push(json!({ "step": step, "status": "warning", "detail": detail }));
    }

    /// A step is starting: only observers hear of it (see `progress`); the record is unchanged.
    fn begin(&self, step: &str) {
        crate::progress::started(step);
    }

    /// `[skills] dir`: link the stack's skills after install. Every outcome is a warning at
    /// worst; the returned messages are the result's `warnings`.
    fn skills(&mut self, ctx: &Ctx, report: &Report) -> Vec<String> {
        let Some(dir) = report.skills_dir.as_deref() else { return Vec::new() };
        let empty = crate::lock::Lockfile::new(Vec::new(), Vec::new(), Vec::new());
        let sync = crate::skills::sync_step(&ctx.cache, &ctx.root, dir, report.lock.as_ref().unwrap_or(&empty), &report.versions);
        let mut warnings: Vec<String> = sync
            .warnings
            .iter()
            .map(|w| format!("skills: {}: {}", w["code"].as_str().unwrap_or_default(), w["message"].as_str().unwrap_or_default()))
            .collect();
        // The links just made or removed.
        warnings.extend(crate::ignore::update(&ctx.root, Some(dir)).map(|w| format!("skills: {w}")));
        let detail = serde_json::to_value(&sync).expect("sync report serializes");
        if warnings.is_empty() { self.ok("skills", detail) } else { self.warn("skills", detail) }
        warnings
    }

    /// Attach the steps that already ran, and whether repeating the command is safe.
    fn fail(mut self, step: &str, err: StackError, changed: bool) -> StackError {
        crate::progress::notify(crate::progress::Event { step, phase: crate::progress::Phase::Failed, code: Some(err.code), detail: None });
        self.0
            .push(json!({ "step": step, "status": "failed", "code": err.code }));
        let mut err =
            err.with_detail(json!({ "steps": self.0, "retry_safe": true, "changed": changed }));
        if changed && err.hint.is_none() {
            err.hint = Some(
                "services may be running; `stack status` shows them, `stack down` stops them"
                    .into(),
            );
        }
        err
    }
}

#[derive(Debug, Clone, Default)]
pub struct LeaseOptions {
    pub ttl_secs: Option<u64>,
    pub owner_pid: Option<u32>,
}

/// Largest owner PID a lease accepts. `pid_t` is a signed 32-bit integer on supported
/// platforms; 0 and larger values name a process group, every process, or nothing, so a
/// lease on them would never expire or would expire immediately depending on the platform.
pub const MAX_OWNER_PID: u32 = i32::MAX as u32;

/// Validate an owner PID from an untyped source (MCP) without truncation.
pub fn owner_pid(value: u64) -> Result<u32> {
    u32::try_from(value)
        .ok()
        .filter(|pid| (1..=MAX_OWNER_PID).contains(pid))
        .ok_or_else(|| {
            StackError::new(
                "usage",
                format!("owner_pid {value} is not a process ID (1-{MAX_OWNER_PID})"),
            )
        })
}

/// What `up` did. Serialized compactly (see [`StartedView`]): the session's full record stays in
/// `.stack/session.json`.
#[derive(Debug)]
pub struct UpReport {
    pub session: Session,
    pub checks: Vec<Check>,
    pub steps: Vec<Value>,
    pub reaped: Vec<GcEntry>,
    /// Problems that did not stop startup (the `gc` and `skills` steps).
    pub warnings: Vec<String>,
    /// Ports `--reassign-ports` moved after a port conflict.
    pub reassigned_ports: Vec<ReassignedPort>,
    /// What happened to each service's init tasks (see `init_tasks`).
    pub init: IndexMap<String, crate::init_tasks::Outcome>,
}

impl Serialize for UpReport {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        StartedView {
            session: SessionView::of(&self.session),
            lease: Some(LeaseSummary::of(self.session.lease.as_ref())),
            restarted: None,
            checks: &self.checks,
            steps: compact_steps(&self.steps),
            reaped: &self.reaped,
            warnings: &self.warnings,
            reassigned_ports: &self.reassigned_ports,
            init: Some(&self.init).filter(|i| !i.is_empty()),
        }
        .serialize(s)
    }
}

/// The result of a successful `up` or `restart`, as agents read it: one check per service
/// (port, readiness, identity, anything withheld) and the session's id and lease. Internals
/// that only stack itself reads (digests, provider IDs, data directories, timestamps of each
/// service) are left to the session file, and steps keep only details that say something the
/// checks do not.
#[derive(Serialize)]
struct StartedView<'a> {
    session: SessionView<'a>,
    /// The session's lease, structured (`up` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    lease: Option<LeaseSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restarted: Option<&'a [String]>,
    checks: &'a [Check],
    steps: Vec<Value>,
    #[serde(skip_serializing_if = "<[GcEntry]>::is_empty")]
    reaped: &'a [GcEntry],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    warnings: &'a [String],
    #[serde(skip_serializing_if = "<[ReassignedPort]>::is_empty")]
    reassigned_ports: &'a [ReassignedPort],
    /// What happened to each service's init tasks (`up` only; see `init_tasks`).
    #[serde(skip_serializing_if = "Option::is_none")]
    init: Option<&'a IndexMap<String, crate::init_tasks::Outcome>>,
}

#[derive(Serialize)]
struct SessionView<'a> {
    id: &'a str,
    started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease: Option<&'a Lease>,
    /// The labels `up --label` recorded, so the caller sees what the session now carries.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: &'a BTreeMap<String, String>,
}

impl<'a> SessionView<'a> {
    fn of(session: &'a Session) -> Self {
        Self { id: &session.id, started_at: session.started_at, lease: session.lease.as_ref(), labels: &session.labels }
    }
}

/// Successful steps without details the result already carries elsewhere (ports and identities
/// are in the checks) or that only matter when something fails (the socket, install coverage:
/// `stack install` reports it). What `stop_previous` and `ports` stopped without a record is kept,
/// and so are the other checkouts' sessions `gc` skipped because they were busy.
fn compact_steps(steps: &[Value]) -> Vec<Value> {
    const REDUNDANT: [&str; 7] = ["gc", "compile", "preflight", "install", "stop", "start", "verify"];
    let redundant = |step: &Value| REDUNDANT.contains(&step["step"].as_str().unwrap_or_default()) && step["detail"].get("skipped").is_none();
    steps
        .iter()
        .map(|step| match step.as_object() {
            Some(fields) if step["status"] == "ok" && (redundant(step) || step["detail"].is_null()) => {
                Value::Object(fields.iter().filter(|(k, _)| *k != "detail").map(|(k, v)| (k.clone(), v.clone())).collect())
            }
            _ => step.clone(),
        })
        .collect()
}

/// Preserve every observed launch identity before further provider calls can fail.
fn record_launch_observations(ctx: &Ctx, statuses: &[DaemonStatus]) -> Result<()> {
    let Some(mut launch) = load(ctx)?.filter(|session| session.launching) else { return Ok(()) };
    let before = launch.clone();
    observe(&mut launch, statuses);
    // A launch record written with these observations already (see `up`) is not written again.
    if serde_json::to_value(&launch).ok() == serde_json::to_value(&before).ok() {
        return Ok(());
    }
    save(ctx, &launch)
}

/// What `statuses` show of `launch`'s services: the supervisor's id, pid and data directory of
/// each one on its recorded port.
fn observe(launch: &mut Session, statuses: &[DaemonStatus]) {
    for daemon in statuses {
        if let Some(record) = launch.services.get_mut(&daemon.name) {
            if daemon.port != Some(record.port) { continue; }
            record.provider_id = daemon.id.clone().or(record.provider_id.take());
            record.pid = daemon.pid.filter(|pid| (1..=MAX_OWNER_PID).contains(pid)).or(record.pid);
            record.data_dir = daemon.data_dir.clone().or(record.data_dir.take());
        }
    }
}

/// Start and verify every service, all within `timeout`: GC, waiting for the project lock,
/// compiling, installing, starting and verifying. When time runs out the current step is cut
/// short (its subprocesses killed with their process groups) and the result is `timed_out`;
/// ownership of anything launched stays recorded, as for any failed launch.
pub fn up(ctx: &Ctx, lease: LeaseOptions, timeout: Duration) -> Result<UpReport> {
    up_with(ctx, lease, timeout, UpOptions::default())
}

fn up_within(ctx: &Ctx, lease: LeaseOptions, opts: &UpOptions) -> Result<UpReport> {
    if let Some(pid) = lease.owner_pid {
        owner_pid(pid.into())?;
    }
    let mut steps = Steps::default();
    let mut timings = Timings::new("up");
    let (reaped, gc_warnings) = gc_pass::before_up(&ctx.state, &mut steps);
    timings.mark("gc");

    let _guard = project_lock(&ctx.state, &ctx.root).map_err(|e| steps.clone().fail("lock", e, false))?;
    timings.mark("lock");
    let mut previous = load(ctx)?;
    if let Some(session) = previous.as_mut() {
        settle_executions(ctx, session).map_err(|e| steps.clone().fail("session", e, false))?;
    }
    let previous = previous;
    if let Some(session) = previous.as_ref().filter(|s| has_active_executions(s)) {
        return Err(steps.fail("session", busy(session, "up"), false));
    }
    // A mise too old for what the configuration asks would silently ignore it: checked before
    // the compile writes anything. The artifact policy is checked, and the provider's lock
    // rendered from stack.lock, before any install.
    steps.begin("compile");
    let report = match ctx.compile_to_install() {
        Ok(r) => r,
        Err((step, e)) => return Err(steps.fail(step, e, false)),
    };
    steps.ok("compile", json!({ "ports": report.ports }));
    timings.mark("compile");
    if let Err(e) = refuse_root(&report) {
        return Err(steps.fail("install", e, false));
    }
    if let Err(e) = crate::init_tasks::check_options(&report.stack, opts).and_then(|()| crate::snapshot::check_up(ctx, &report, opts)) {
        return Err(steps.fail("compile", e, false));
    }

    let plan = match prepare_install(ctx, &report) {
        Ok(plan) => plan,
        Err(e) => return Err(steps.fail("install", e, false)),
    };
    let recorded_trust = match mise::trust_once(&ctx.state, &ctx.root) {
        Ok(skipped) => skipped,
        Err(e) => return Err(steps.fail("install", e, false)),
    };
    steps.begin("install");
    match install_partitioned(ctx, &plan, &report, recorded_trust) {
        Ok(detail) => steps.ok("install", detail),
        Err(e) => return Err(steps.fail("install", e, false)),
    }
    timings.mark("install");
    let mut warnings = steps.skills(ctx, &report);
    // Recorded before anything starts, so a deleted project's services can still be found.
    let provider = (!report.stack.services.is_empty()).then(|| ProviderRecord::native(supervisor_dir(ctx)));
    let project_dir_id = dir_id(&ctx.root);
    let project_dir_created = dir_created(&ctx.root);

    // `start` can reuse an already-running daemon with an old definition. Stop it first
    // when changing generations, or when no launch record establishes its configuration.
    let digest = config_digest(ctx, &report);
    // Services an older Stack started under Pitchfork are stopped there and started again under
    // Stack's own supervisor, with the same data.
    let legacy = legacy_provider(previous.as_ref()).cloned();
    let restart = legacy.is_some() || previous.as_ref().is_some_and(|s| !same_config(s, ctx, &report));
    let unrecorded = previous.is_none() && !report.stack.services.is_empty();
    if restart || unrecorded {
        match down_locked(ctx) {
            Err(e) => return Err(steps.fail("stop_previous", e, true)),
            // Processes no record accounted for are worth saying; the rest is routine.
            Ok(down) => {
                let unrecorded: Vec<&Value> = down.stopped.iter().filter(|s| s["unrecorded"] == true).collect();
                let mut detail = if unrecorded.is_empty() { json!(null) } else { json!({ "stopped": unrecorded }) };
                if let Some(legacy) = legacy {
                    let outcome = stop_owned_supervisor(ctx, Some(legacy.clone()));
                    pitchfork_outcome(ctx, &legacy, &outcome, &mut warnings);
                    detail = json!({ "stopped": unrecorded, "pitchfork": outcome });
                }
                steps.ok("stop_previous", detail);
            }
        }
        timings.mark("stop_previous");
    }
    // Nothing can start on a port a foreign program holds, and the supervisor's own error
    // would come after the attempt. Stack's own daemons still running (an unchanged
    // generation) are not conflicts.
    // The supervisor's listing, asked once for the port check and the launch record: nothing
    // between them starts or stops a daemon it tracks.
    let mut listed = None;
    if !report.stack.services.is_empty() {
        let statuses = listing(ctx, &report);
        // This checkout's own service processes that no record accounts for are stopped, not
        // refused as another program's (see `Stray`).
        let token = crate::checkout::token(&ctx.root);
        let exempt = commands::Exempt::load(&ctx.state, Some(&ctx.root));
        let (mine, conflicts): (Vec<Stray>, Vec<PortConflict>) = port_conflicts(&report, &statuses).into_iter().fold(
            (Vec::new(), Vec::new()),
            |(mut mine, mut others), c| {
                match token.as_deref().and_then(|t| stray(&c.service, c.port, t, &exempt)) {
                    Some(s) => mine.push(s),
                    None => others.push(c),
                }
                (mine, others)
            },
        );
        let survivors = stop_strays(&mine, &[], None);
        if !survivors.is_empty() {
            let e = StackError::new("stop_unconfirmed", "a service process the supervisor no longer tracks did not stop")
                .hint("run `stack down`, then `stack up`")
                .details(survivors);
            return Err(steps.fail("ports", e, false));
        }
        if !conflicts.is_empty() {
            return Err(steps.fail("ports", port_conflict_error(conflicts), false));
        }
        steps.ok("ports", if mine.is_empty() { json!(null) } else { json!({ "stopped": mine.iter().map(Stray::json).collect::<Vec<_>>() }) });
        timings.mark("ports");
        match claim_data(ctx, &report, &statuses, provider.as_ref()) {
            Ok(moved) => warnings.extend(moved),
            Err(e) => return Err(steps.fail("data", e, false)),
        }
        timings.mark("data");
        // After the claim: each data directory is recorded for this checkout and marked as its.
        if let Some(name) = opts.from_snapshot.as_deref() {
            match crate::snapshot::restore_before_start(ctx, &report, &statuses, name) {
                Ok(detail) => steps.ok("restore_snapshot", detail),
                Err(e) => return Err(steps.fail("restore_snapshot", e, false)),
            }
        }
        // Stopping this checkout's untracked processes can change what the supervisor says
        // about the daemons they belonged to: asked again then.
        if mine.is_empty() {
            listed = Some(statuses);
        }
    }

    let stamp = now();
    let launched_at = now_ms();
    // The labels this checkout's session had, with those given now added or replacing them.
    let mut labels = previous.as_ref().map(|p| p.labels.clone()).unwrap_or_default();
    labels.extend(opts.labels.iter().map(|(k, v)| (k.clone(), v.clone())));
    let lease = (lease.ttl_secs.is_some() || lease.owner_pid.is_some()).then_some(Lease {
        ttl_secs: lease.ttl_secs,
        owner_pid: lease.owner_pid,
        renewed_at: stamp,
    });
    let mut checks = Vec::new();
    if !report.stack.services.is_empty() {
        // Ownership is recorded before anything starts. If start or verification fails, or the
        // service is later removed from the configuration, `down` still knows what to reconcile.
        let unverified = restart || previous.as_ref().map_or(true, |p| p.launching || !same_config(p, ctx, &report));
        // Retrying an incomplete launch of this same generation stopped nothing, so whatever it
        // recorded (a process an interrupted `up` or `restart` observed) may still run. Those
        // identifiers are kept until observations replace them or `down` confirms them gone.
        let carried = previous.as_ref().filter(|p| p.launching && !restart);
        // Capture what runs before launch, including interrupted starts.
        let before = listed.take().unwrap_or_else(|| listing(ctx, &report));
        if unverified {
            let mut launch = Session {
                id: new_session_id(ctx),
                project: ctx.root.clone(),
                lock_digest: lock_digest(&ctx.root),
                config_digest: digest.clone(),
                active_executions: IndexMap::new(),
                started_at: stamp,
                launching: true,
                lease: lease.clone(),
                services: report
                    .ports
                    .iter()
                    .map(|(name, port)| {
                        let record = carried
                            .and_then(|p| p.services.get(name))
                            .filter(|r| r.port == *port)
                            .map(|r| ServiceRecord { identity: Identity::Liveness, verified_at: 0, ..r.clone() })
                            .unwrap_or(ServiceRecord { port: *port, pid: None, data_dir: None, identity: Identity::Liveness, verified_at: 0, started_at_ms: None, provider_id: None });
                        (name.clone(), record)
                    })
                    .collect(),
                provider: provider.clone(),
                project_dir_id,
                project_dir_created,
                labels: labels.clone(),
            };
            // Written once, with what the supervisor already shows (one synced write fewer).
            observe(&mut launch, &before);
            save(ctx, &launch).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        } else if let Some(previous) = previous.as_ref().filter(|p| p.provider != provider) {
            // The same generation, started again through the supervisor this environment
            // reaches now: the record names it before anything starts there.
            let moved = Session { provider: provider.clone(), ..previous.clone() };
            save(ctx, &moved).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        }
        record_launch_observations(ctx, &before).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        timings.mark("record_launch");
        let names: Vec<String> = report.stack.services.keys().cloned().collect();
        // A service still running whose dependency is about to start again (it died, or never
        // ran) would keep its connections to the old one: stopped first, so it starts after it.
        let running = |name: &String| before.iter().any(|d| d.name == *name && matches!(d.status.as_str(), "running" | "starting") && d.pid.is_some_and(pid_alive));
        let starting: Vec<String> = names.iter().filter(|n| !running(n)).cloned().collect();
        let stale: Vec<String> = dependents_of(&report.stack.services, &starting).into_iter().filter(|n| running(n)).collect();
        if !stale.is_empty() {
            match stop_and_wait(ctx, &stale, previous.as_ref()) {
                Ok(()) => steps.ok("stop_dependents", json!(stale)),
                Err(e) => return Err(steps.fail("stop_dependents", e, true)),
            }
        }
        // A verified session of this same generation, reached through the supervisor it
        // recorded, whose services that supervisor all reports running, each with a live
        // process, has nothing to start: a request would return without doing anything, after
        // a round trip to the provider for every service. A start of no services still does
        // what comes before the first request (the supervisor is started if it is not running,
        // watched as any start is, so one that hangs holds nothing back). Whether each service
        // is still this checkout's instance is for the verification below, as after a start.
        // Anything else (a service that died, is still starting, or was never verified, or a
        // session moving to another supervisor) is requested as before, and so is every stack
        // whose services depend on one another: starting those in order checks that each
        // dependency still passes its declared readiness, which nothing here does.
        let settled = !unverified
            && dependency_waves(&report.stack.services, &names).len() <= 1
            && previous.as_ref().is_some_and(|p| p.provider == provider)
            && names.iter().all(|n| before.iter().any(|d| d.name == *n && d.status == "running" && d.pid.is_some_and(pid_alive)));
        steps.begin("start");
        let started = if settled { Ok(()) } else { start_in_order(ctx, &report, &names) };
        if let Err(start_error) = started {
            let observed = record_after_failed_start(ctx, &report);
            if let Err(error) = observed {
                return Err(steps.fail("record_partial_start", error.with_detail(json!({ "start_error": start_error })), true));
            }
            return Err(steps.fail("start", with_readiness_unless_cancelled(ctx, &report, &names, start_error), true));
        }
        steps.ok("start", json!(null));
        timings.mark("start");

        steps.begin("verify");
        let env;
        (checks, env) = verify_launch(ctx, &report, "up_verify").map_err(|(step, e)| steps.clone().fail(step, with_readiness(ctx, &report, &names, e), true))?;
        steps.ok(
            "verify",
            json!(checks
                .iter()
                .map(|c| (c.service.clone(), c.identity))
                .collect::<IndexMap<_, _>>()),
        );
        timings.mark("verify");
        warnings.extend(encoding_warnings(&report, &env, &checks));
        bind_data(ctx, &checks);
        timings.mark("encoding_bind");
    }

    // Startup and verification are not idle time: the lease starts once services are verified.
    let stamp = now();
    let lease = lease.map(|l| Lease { renewed_at: stamp, ..l });
    // The same verified generation, still running: keep its identity so earlier callers'
    // records stay valid.
    let reused = previous
        .as_ref()
        .filter(|p| !p.launching && same_config(p, ctx, &report) && !restart && checks_match(p, &checks))
        .map(|p| (p.id.clone(), p.started_at));
    let session = Session {
        id: reused.as_ref().map_or_else(|| new_session_id(ctx), |(id, _)| id.clone()),
        project: ctx.root.clone(),
        lock_digest: lock_digest(&ctx.root),
        config_digest: digest,
        active_executions: IndexMap::new(),
        started_at: reused.map_or(stamp, |(_, started)| started),
        launching: false,
        lease,
        services: checks
            .iter()
            .map(|c| {
                // A process the previous record already knew keeps its start time, unknown for
                // records older than start times; anything else was started by this call.
                let started = match previous.as_ref().and_then(|p| p.services.get(&c.service)) {
                    Some(r) if r.pid.is_some() && r.pid == c.pid => r.started_at_ms,
                    _ => Some(launched_at),
                };
                (c.service.clone(), record_of(c, started))
            })
            .collect(),
        provider: if report.stack.services.is_empty() { None } else { provider },
        project_dir_id,
        project_dir_created,
        labels,
    };
    // The same verified instance found running again changes only when it was last verified
    // and renewed: the durable record before this one describes it as well, so this one is not
    // waited for on disk (as `exec` and `status` renew). Anything else is.
    let durable = !previous.as_ref().is_some_and(|p| same_instance(p, &session));
    save_with(ctx, &session, durable).map_err(|e| steps.clone().fail("record_session", e, true))?;
    timings.mark("record_session");
    let mut up = UpReport {
        session,
        checks,
        steps: steps.0,
        reaped,
        warnings: gc_warnings.into_iter().chain(warnings).chain(unconfirmed_down(ctx).map(|_| {
            "the last `stack down` here was not confirmed: what it reported may still run beside these services, holding their ports or data; `stack down` checks again".to_string()
        })).collect(),
        reassigned_ports: Vec::new(),
        init: IndexMap::new(),
    };
    // Init tasks run like `stack run`, which takes the project lock itself.
    drop(_guard);
    crate::init_tasks::run(ctx, &mut up, &report.stack, opts)?;
    Ok(up)
}

/// Whether `new` records the same finished launch as `previous`, apart from when its services
/// were last verified and its lease renewed.
fn same_instance(previous: &Session, new: &Session) -> bool {
    let without_times = |session: &Session| {
        let mut v = serde_json::to_value(session).unwrap_or_default();
        if let Some(services) = v.get_mut("services").and_then(Value::as_object_mut) {
            for record in services.values_mut().filter_map(Value::as_object_mut) {
                record.remove("verified_at");
            }
        }
        if let Some(lease) = v.get_mut("lease").and_then(Value::as_object_mut) {
            lease.remove("renewed_at");
        }
        v
    };
    !previous.launching && !new.launching && without_times(previous) == without_times(new)
}

/// A warning for each verified Postgres preset whose cluster is not `UTF8`: one created under
/// a non-UTF-8 locale before Stack fixed the locale of new clusters (see
/// [`mise::preset_env`]). Its data is kept as it is, never reinitialized; the warning says
/// how to move to a `UTF8` cluster. Best effort: nothing is asked when no Postgres verified,
/// and a query that fails warns about nothing.
///
/// `env` is the environment the checks were verified with, so the provider is not asked again.
fn encoding_warnings(report: &Report, env: &IndexMap<String, String>, checks: &[Check]) -> Vec<String> {
    checks
        .iter()
        .filter(|c| c.ready && report.stack.services.get(&c.service).is_some_and(|s| s.value.preset.as_deref() == Some("postgres")))
        .filter_map(|check| encoding_warning(env, check))
        .collect()
}

/// [`encoding_warnings`] for one verified Postgres, asked over the app's `DATABASE_URL`.
fn encoding_warning(env: &IndexMap<String, String>, check: &Check) -> Option<String> {
    let url = env.get("DATABASE_URL")?;
    let query = "show server_encoding";
    let encoding = match crate::wire::postgres(url, env, query) {
        crate::wire::Asked::Answer(out) => out,
        crate::wire::Asked::Fallback(_) => run_probe(env, "psql", &["-X", url, "-Atc", query]).ok()?,
        crate::wire::Asked::Failed(_) => return None,
    };
    let encoding = encoding.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    if encoding == "UTF8" {
        return None;
    }
    let dir = check.data_dir.as_deref().unwrap_or("its data directory");
    Some(format!(
        "{}: the cluster in {dir} uses encoding {encoding}, so text is not checked as UTF-8 and clients such as psycopg return bytes; \
         it was created under a non-UTF-8 locale and is kept as it is. New clusters are UTF8 (locale {}). To move this one: \
         `stack exec -- sh -c 'pg_dumpall -d \"$DATABASE_URL\" > dump.sql'`, `stack down`, move {dir} aside, `stack up`, \
         then `stack exec -- sh -c 'psql \"$DATABASE_URL\" -f dump.sql'`",
        check.service,
        mise::POSTGRES_LOCALE
    ))
}

/// What [`data_marker`] adds to the data directory's name.
pub(crate) const MARKER_SUFFIX: &str = ".stack-checkout";

/// The file beside a service's data directory naming the checkout (by token, see
/// [`crate::checkout`]) whose data it is.
pub(crate) fn data_marker(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(MARKER_SUFFIX);
    dir.with_file_name(name)
}

/// Make the data directories of the services about to start this checkout's. mise keeps a
/// preset's data under a directory keyed by the project's path alone, so a checkout deleted
/// and recreated at the same path would find its predecessor's databases. Data marked for
/// another checkout is moved aside, never deleted, to `<dir>.replaced-<unix time>` with its
/// marker, and reported; this checkout then starts with empty data. Data of this checkout stays.
/// Data from before markers existed (or with an unreadable marker) cannot be told apart and is
/// adopted. Running services are
/// left alone: their data is in use.
fn claim_data(ctx: &Ctx, report: &Report, statuses: &[DaemonStatus], provider: Option<&ProviderRecord>) -> Result<Vec<String>> {
    let Some(token) = crate::checkout::token(&ctx.root) else { return Ok(Vec::new()) };
    let mut moved = Vec::new();
    // Recorded together, with one write of the machine-wide record; what was claimed before a
    // failure is recorded all the same.
    let mut claimed = Vec::new();
    let result = claim_each(ctx, report, statuses, provider, &token, &mut moved, &mut claimed);
    let recorded = crate::datadirs::record_all(&ctx.state, claimed);
    result.and(recorded).map(|()| moved)
}

/// [`claim_data`] for each daemon, collecting the entries to record in `claimed`.
fn claim_each(
    ctx: &Ctx,
    report: &Report,
    statuses: &[DaemonStatus],
    provider: Option<&ProviderRecord>,
    token: &str,
    moved: &mut Vec<String>,
    claimed: &mut Vec<crate::datadirs::Entry>,
) -> Result<()> {
    let token = token.to_string();
    for daemon in statuses {
        let Some(dir) = daemon.data_dir.as_deref().map(PathBuf::from) else { continue };
        let running = matches!(daemon.status.as_str(), "running" | "starting") || daemon.pid.is_some_and(pid_alive);
        if !report.stack.services.contains_key(&daemon.name) {
            continue;
        }
        let marker = data_marker(&dir);
        let owner = data_owner(&dir);
        // Where this checkout's data is, so `gc` can find it once the checkout is deleted.
        let supervisor = provider.map(|p| crate::datadirs::Supervisor { pitchfork: p.pitchfork.clone(), state_dir: p.state_dir.clone(), id: daemon.id.clone().unwrap_or_else(|| daemon.name.clone()) });
        let (location, identity) = crate::datadirs::locate(&dir);
        let entry = crate::datadirs::Entry { project: ctx.root.clone(), service: daemon.name.clone(), dir: dir.clone(), checkout: token.clone(), supervisor, location, identity };
        if running || owner.as_deref() == Some(token.as_str()) {
            if owner.as_deref() == Some(token.as_str()) {
                claimed.push(entry);
            }
            continue;
        }
        if owner.is_some() && dir.exists() {
            let stamp = now();
            let aside = (0..)
                .map(|n| {
                    let suffix = if n == 0 { format!(".replaced-{stamp}") } else { format!(".replaced-{stamp}-{n}") };
                    let mut name = dir.file_name().unwrap_or_default().to_os_string();
                    name.push(suffix);
                    dir.with_file_name(name)
                })
                .find(|p| !p.exists())
                .expect("an unused name");
            fs::rename(&dir, &aside).map_err(|e| io_error(dir.display(), e))?;
            fs::rename(&marker, data_marker(&aside)).map_err(|e| io_error(marker.display(), e))?;
            moved.push(format!(
                "{}: data an earlier checkout at this path left in {} was moved to {}; this checkout starts with empty data",
                daemon.name,
                dir.display(),
                aside.display()
            ));
        }
        crate::state::write_json(&marker, &json!({ "checkout": token }))?;
        claimed.push(entry);
    }
    Ok(())
}

/// The checkout token a data directory's marker names. An unreadable marker says no more than
/// a missing one.
fn data_owner(dir: &Path) -> Option<String> {
    fs::read(data_marker(dir)).ok().and_then(|bytes| marker_owner(&bytes))
}

pub(crate) fn marker_owner(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(bytes).ok().and_then(|v| v["checkout"].as_str().map(String::from))
}

/// Bind the data directories of verified services, which a first start may only now have
/// created, to where they resolve and which directories they are (see
/// [`crate::datadirs::bind`]), when their markers name this checkout. Best effort: data whose
/// directory was never bound is kept by `gc`, never removed.
fn bind_data(ctx: &Ctx, checks: &[Check]) {
    let Some(token) = crate::checkout::token(&ctx.root) else { return };
    let dirs: Vec<&Path> = checks
        .iter()
        .filter(|c| c.ready)
        .filter_map(|c| c.data_dir.as_deref().map(Path::new))
        .filter(|dir| data_owner(dir).as_deref() == Some(token.as_str()))
        .collect();
    let _ = crate::datadirs::bind_all(&ctx.state, &dirs, &token);
}

/// Start `names` in the order [`dependency_waves`] gives: each wave of services is started
/// together, and waited for until it is ready, before the next, so a service starts only once
/// the services it `depends_on` run. A stack without dependencies among `names` is one wave.
///
/// A signal stack passed on to a setup (see [`cancelled`]) ends it there: no later setup runs
/// and nothing more is started, however the setup itself exited.
fn start_in_order(ctx: &Ctx, report: &Report, names: &[String]) -> Result<()> {
    let waves = dependency_waves(&report.stack.services, names);
    let last = waves.len().saturating_sub(1);
    for (i, wave) in waves.iter().enumerate() {
        let later = || json!({ "not_started": waves[i + 1..].iter().flatten().collect::<Vec<_>>() });
        prerequisites_ready(ctx, report, wave, names).map_err(|e| if waves.len() > 1 { e.with_detail(later()) } else { e })?;
        run_setups(ctx, report, wave).and_then(|()| cancelled(wave)).map_err(|e| if e.code == "cancelled" && waves.len() > 1 { e.with_detail(later()) } else { e })?;
        start_wave(ctx, report, wave)?;
        // What depends on this wave starts only once Stack verified it and its declared
        // readiness passes.
        if i < last {
            await_readiness(ctx, report, wave).map_err(|e| {
                let waiting: Vec<&String> = waves[i + 1..].iter().flatten().collect();
                e.with_detail(json!({ "not_started": waiting }))
            })?;
        }
    }
    Ok(())
}

/// Have the supervisor run `names`, with every service's definition as `report` and the
/// provider's environment make it now. Returns once they are spawned.
fn start_wave(ctx: &Ctx, report: &Report, names: &[String]) -> Result<()> {
    let env = provider_env(ctx, report)?;
    let specs = service_specs(ctx, report, &env)?;
    let mut timings = Timings::new("start");
    supervisor::start(&supervisor_dir(ctx), &ctx.root, &specs, names)?;
    timings.mark("spawn");
    Ok(())
}

/// `cancelled` once stack passed an interrupt, terminate or hangup signal on to a command it ran
/// (a setup) instead of ending: the caller cancelled this start, so `names`, not yet requested,
/// must not be. The signal is stack's to honour whatever the command did with it (a setup may
/// handle it and exit 0); the CLI then exits 128 + the signal, and the MCP server ends once it
/// answered.
fn cancelled(names: &[String]) -> Result<()> {
    match crate::process::passed_on() {
        None => Ok(()),
        Some(signal) => Err(cancellation(signal, None, names)),
    }
}

fn cancellation(signal: i32, setup: Option<&str>, names: &[String]) -> StackError {
    let which = match signal {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".into(),
        15 => "SIGTERM".into(),
        n => format!("signal {n}"),
    };
    let during = setup.map(|s| format!(" during {s}'s setup")).unwrap_or_default();
    StackError::new("cancelled", format!("stack received {which}{during}, so the start was cancelled: {} not started", names.join(", ")))
        .hint("nothing more was started; anything launched before stays recorded: `stack status` shows it, `stack down` stops it, `stack up` starts again")
        .with_detail(json!({ "signal": signal, "setup": setup, "not_started": names }))
}

/// Most a service's `setup` may take, within the startup deadline.
const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
/// Bytes of each output stream a failed `setup` keeps.
const SETUP_OUTPUT_LIMIT: usize = 64 * 1024;

/// Run the `setup` of each of `names` that is about to start (the supervisor does not report it
/// running): through `sh -c` in the project, with the stack's tools and environment and the
/// service's `$PORT`, as `stack exec` would, bounded by the startup deadline. The services
/// it depends on were started and verified before it. A failing setup is `setup_failed`
/// with its exit status and last lines, and the service is not started.
fn run_setups(ctx: &Ctx, report: &Report, names: &[String]) -> Result<()> {
    let wanted: Vec<(&String, &String)> = names
        .iter()
        .filter_map(|n| Some((n, report.stack.services.get(n)?.value.setup.as_ref()?)))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let statuses = listing(ctx, report);
    let running = |name: &String| statuses.iter().any(|d| d.name == *name && matches!(d.status.as_str(), "running" | "starting") && d.pid.is_some_and(pid_alive));
    let wanted: Vec<_> = wanted.into_iter().filter(|(n, _)| !running(n)).collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let env = provider_env(ctx, report)?;
    for (name, setup) in wanted {
        let mut sh = Command::new("sh");
        sh.args(["-c", setup]).current_dir(&ctx.root);
        mise::configure_command(&mut sh, &ctx.root);
        sh.envs(&env);
        if let Some(port) = report.ports.get(name) {
            sh.env("PORT", port.to_string());
        }
        cancelled(names)?;
        let out = crate::process::capture_redacted(&mut sh, crate::process::bounded(SETUP_TIMEOUT), SETUP_OUTPUT_LIMIT, None)
            .map_err(|e| StackError::new("setup_failed", format!("cannot run {name}'s setup: {e}")))?;
        // Before its exit status: a setup that handled the signal and exited 0 did not succeed.
        if let Some(signal) = crate::process::passed_on() {
            return Err(cancellation(signal, Some(name), names).with_detail(json!({ "exit_code": out.exit_code, "signal_ended_setup": out.signal })));
        }
        if out.exit_code == Some(0) && !out.timed_out {
            continue;
        }
        if out.timed_out && crate::process::expired() {
            return Err(StackError::new("timed_out", format!("{name}'s setup was cut short by the deadline")));
        }
        let tail = |text: &str| -> String {
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            truncate(&lines[lines.len().saturating_sub(12)..].join("\n"), 2000)
        };
        let how = match (out.timed_out, out.exit_code) {
            (true, _) => format!("did not finish within {}s", SETUP_TIMEOUT.as_secs()),
            (false, Some(code)) => format!("exited {code}"),
            (false, None) => "was ended by a signal".to_string(),
        };
        return Err(StackError::new("setup_failed", format!("{name}'s setup {how}, so {name} was not started"))
            .hint(format!("fix it and run `stack up` again; `stack exec -- sh -c {}` runs it by hand", shell_quote(setup)))
            .with_detail(json!({ "service": name, "setup": setup, "exit_code": out.exit_code, "timed_out": out.timed_out, "stdout": tail(&out.stdout), "stderr": tail(&out.stderr) })));
    }
    Ok(())
}

/// `names` grouped by dependency depth among themselves (no dependency on another of `names`
/// first), each group in declaration order.
fn dependency_waves(services: &IndexMap<String, Entry<Service>>, names: &[String]) -> Vec<Vec<String>> {
    fn depth(services: &IndexMap<String, Entry<Service>>, names: &[String], name: &str, seen: &mut Vec<String>) -> usize {
        if seen.iter().any(|s| s == name) {
            return 0; // compile refuses cycles; never loop
        }
        seen.push(name.to_string());
        let deps = services.get(name).map(|e| e.value.depends_on.clone()).unwrap_or_default();
        let d = deps.iter().filter(|d| names.contains(d)).map(|d| 1 + depth(services, names, d, seen)).max().unwrap_or(0);
        seen.pop();
        d
    }
    let mut waves: Vec<Vec<String>> = Vec::new();
    for name in services.keys().filter(|n| names.contains(n)) {
        let d = depth(services, names, name, &mut Vec::new());
        if waves.len() <= d {
            waves.resize(d + 1, Vec::new());
        }
        waves[d].push(name.clone());
    }
    waves.retain(|w| !w.is_empty());
    waves
}

/// Every service one of `names` depends on, directly or through others, not in `names`.
fn prerequisites_of(services: &IndexMap<String, Entry<Service>>, names: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut next: Vec<String> = names.to_vec();
    while let Some(name) = next.pop() {
        for dep in services.get(&name).map(|e| e.value.depends_on.clone()).unwrap_or_default() {
            if !names.contains(&dep) && !found.contains(&dep) {
                found.push(dep.clone());
                next.push(dep);
            }
        }
    }
    services.keys().filter(|n| found.contains(n)).cloned().collect()
}

/// Fail, before anything of `starting` is set up or started, unless every service it depends
/// on that is not among `names` (whose own starts come first, in waves) runs, verifies and
/// passes its declared readiness. Such a service is never started here: `restart` starts only
/// the services it was given and their dependents, so a prerequisite it was not given must
/// already be up. `stack restart <it>` (which restarts its dependents too) or `stack up`
/// recovers it. Nothing is asked of the supervisor when there is no such service.
fn prerequisites_ready(ctx: &Ctx, report: &Report, starting: &[String], names: &[String]) -> Result<()> {
    let outside: Vec<String> = prerequisites_of(&report.stack.services, starting).into_iter().filter(|p| !names.contains(p)).collect();
    if outside.is_empty() {
        return Ok(());
    }
    let (env, statuses) = (provider_env(ctx, report)?, listing(ctx, report));
    let mut unready = Vec::new();
    for name in &outside {
        let Some(entry) = report.stack.services.get(name) else { continue };
        let status = statuses.iter().find(|d| d.name == *name);
        let token = report.identities.get(name).map(String::as_str);
        let reason = match (verify_one(&ctx.root, &entry.value, report.ports.get(name).copied(), status, &env, token), &entry.value.ready_cmd) {
            (Err((_, reason)), _) => Some((reason, None)),
            // A declared ready_cmd runs once, kept with what it answered when it fails. One that
            // cannot be run says nothing of readiness, so it does not pass either.
            (Ok(_), Some(command)) => match check_ready_cmd(ctx, report, &env, name, command) {
                Ok(()) => None,
                Err(failure) => {
                    let reason = match failure["error"].as_str() {
                        Some(error) => format!("its ready_cmd could not be run, so its readiness is unknown ({command}: {error})"),
                        None => format!("its ready_cmd does not pass ({command})"),
                    };
                    Some((reason, Some(failure)))
                }
            },
            (Ok(_), None) => (declared_ready(ctx, report, &env, name) == Some(false)).then(|| ("its ready_port is not accepting connections".to_string(), None)),
        };
        if let Some((reason, readiness)) = reason {
            let needed_by: Vec<&String> = starting.iter().filter(|s| prerequisites_of(&report.stack.services, &[(*s).clone()]).contains(name)).collect();
            unready.push(json!({ "service": name, "needed_by": needed_by, "reason": reason, "readiness": readiness }));
        }
    }
    let Some(first) = unready.first() else { return Ok(()) };
    let (dep, reason) = (first["service"].as_str().unwrap_or_default(), first["reason"].as_str().unwrap_or_default());
    let needers: Vec<&str> = first["needed_by"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    Err(StackError::new(
        "dependency_not_ready",
        format!("{} depends on {dep}, which is not ready: {reason}; nothing was stopped, set up or started", needers.join(", ")),
    )
    .hint(format!("start it first: `stack restart {dep}` restarts it and what depends on it, `stack up` starts every service that is not running"))
    .with_detail(json!({ "not_ready": unready, "not_started": starting })))
}

/// Every service that depends on one of `names`, directly or through others, not in `names`.
fn dependents_of(services: &IndexMap<String, Entry<Service>>, names: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    loop {
        let more: Vec<String> = services
            .iter()
            .filter(|(name, e)| !names.contains(name) && !found.contains(name) && e.value.depends_on.iter().any(|d| names.contains(d) || found.contains(d)))
            .map(|(name, _)| name.clone())
            .collect();
        if more.is_empty() {
            return found;
        }
        found.extend(more);
    }
}

/// Stop `names` through the supervisor and wait until the processes `session` recorded for them
/// are gone and their ports closed (`stop_unconfirmed` otherwise).
fn stop_and_wait(ctx: &Ctx, names: &[String], session: Option<&Session>) -> Result<()> {
    supervisor::stop(&supervisor_dir(ctx), names)?;
    let deadline = Instant::now() + crate::process::bounded(STOP_TIMEOUT);
    loop {
        let alive: Vec<Value> = names
            .iter()
            .filter_map(|n| session?.services.get(n).map(|r| (n, r)))
            .filter(|(_, r)| r.pid.is_some_and(pid_alive) || accepting(r.port))
            .map(|(n, r)| json!({ "service": n, "pid": r.pid, "port": r.port }))
            .collect();
        if alive.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(StackError::new("stop_unconfirmed", "services did not stop")
                .hint("retry `stack restart`, or `stack down` and `stack up`")
                .details(alive));
        }
        sleep(Duration::from_millis(200));
    }
}

/// Wait, as verification does, for `names` to verify and pass their declared readiness.
fn await_readiness(ctx: &Ctx, report: &Report, names: &[String]) -> Result<()> {
    let window = Instant::now() + READY_TIMEOUT;
    let timings = Timings::new("start_watch");
    let env = provider_env(ctx, report)?;
    let mut seen: Option<Vec<DaemonStatus>> = None;
    let mut pause = Pause::new();
    loop {
        let statuses = listing(ctx, report);
        if seen.as_ref() != Some(&statuses) {
            let _ = record_launch_observations(ctx, &statuses);
            seen = Some(statuses.clone());
        }
        exited_on_start(ctx, &statuses, names)?;
        let starting = statuses.iter().any(|d| names.contains(&d.name) && d.status == "starting");
        let out_of_time = Instant::now() >= window || crate::process::expired();
        let pending: Vec<String> = if starting && !out_of_time {
            names.to_vec()
        } else {
            let checks = verify_some(&ctx.root, report, &env, &statuses, &timings, Some(names));
            names
                .iter()
                .filter(|n| !checks.iter().any(|c| c.service == **n && c.ready) || declared_ready(ctx, report, &env, n) != Some(true))
                .cloned()
                .collect()
        };
        if pending.is_empty() {
            return Ok(());
        }
        if out_of_time {
            return Err(StackError::new("start_failed", format!("{} did not become ready", pending.join(", ")))
                .hint("`stack logs <service>` shows why; services may be running, `stack down` stops them"));
        }
        sleep(crate::process::bounded(pause.next(starting)));
    }
}

/// `name`'s `ready_cmd` once, as the supervisor runs it: through `sh` in the project, with the
/// stack's environment and `$PORT`, bounded at 5 seconds and 4 KiB of output. An error is a
/// check that could not be run or read: it answered nothing, so it never counts as passing.
fn run_ready_cmd(ctx: &Ctx, report: &Report, env: &IndexMap<String, String>, name: &str, command: &str) -> std::io::Result<crate::process::Captured> {
    let mut sh = Command::new("sh");
    sh.args(["-c", command]).current_dir(&ctx.root);
    mise::configure_command(&mut sh, &ctx.root);
    sh.envs(env);
    if let Some(port) = report.ports.get(name) {
        sh.env("PORT", port.to_string());
    }
    crate::process::capture(&mut sh, crate::process::bounded(Duration::from_secs(5)), 4096)
}

/// Whether `name` passes the readiness check its definition declares, as the supervisor would
/// run it (`sh -c` in the project, with the app's environment and `$PORT`). Postgres and
/// Redis without one are checked by verification itself, through their protocol. `None` when
/// stack cannot tell: another preset's check is the provider's own. A `ready_cmd` that cannot
/// be run does not pass.
fn declared_ready(ctx: &Ctx, report: &Report, env: &IndexMap<String, String>, name: &str) -> Option<bool> {
    let service = &report.stack.services.get(name)?.value;
    if let Some(command) = &service.ready_cmd {
        return Some(run_ready_cmd(ctx, report, env, name, command).is_ok_and(|out| !out.timed_out && out.exit_code == Some(0)));
    }
    match service.preset.as_deref() {
        Some("postgres" | "redis") | None => Some(service.ready_port.map_or(true, accepting)),
        Some(_) => None,
    }
}

/// Every service's passing check with the environment they were verified with, or the step
/// that failed and its error.
type Verified = std::result::Result<(Vec<Check>, IndexMap<String, String>), (&'static str, StackError)>;

/// Verify what a start launched, recording each supervisor listing as it is seen. Should the
/// deadline end verification before any listing was seen, what launched is recorded anyway.
fn verify_launch(ctx: &Ctx, report: &Report, timings: &'static str) -> Verified {
    verify_until_ready(ctx, report, timings, |statuses| record_launch_observations(ctx, statuses)).inspect_err(|_| {
        if crate::process::expired() {
            // Best effort: the launch record already names the provider that can stop them.
            let _ = record_after_failed_start(ctx, report);
        }
    })
}

/// Record whatever a failed or cut-short start launched. A start the deadline ended gets a
/// short grace period for this: losing track of a launched service is worse than overrunning.
fn record_after_failed_start(ctx: &Ctx, report: &Report) -> Result<()> {
    let _grace = crate::process::grace_scope(RECORD_GRACE);
    record_launch_observations(ctx, &listing(ctx, report))
}

/// When a startup given `timeout` must end. A timeout too long to fix as a point in time is
/// refused before any work, rather than leaving the startup unbounded.
fn startup_deadline(timeout: Duration) -> Result<Instant> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        StackError::new("usage", format!("a startup timeout of {}s is too long", timeout.as_secs()))
            .hint("give the time startup may take, e.g. 10m or 2h")
    })
}

/// `timed_out` for a startup whose deadline passed, else `error` unchanged. What was cut short
/// is the `cause`; the progress record, if any, stays last as on every failure of `up`.
fn startup_timed_out(mut error: StackError, command: &str, timeout: Duration) -> StackError {
    if !crate::process::expired() {
        return error;
    }
    let progress = error
        .details
        .last()
        .is_some_and(|d| d.get("steps").is_some())
        .then(|| error.details.pop())
        .flatten();
    let changed = progress.as_ref().is_some_and(|p| p["changed"] == true);
    let unready = unready_hint(&error);
    let mut details = vec![json!({ "cause": error })];
    details.extend(progress);
    let hint = match unready {
        Some(unready) => format!(
            "{unready}. Waiting longer helps only if the service is still starting: `stack logs <service>` shows it; \
             fix the service or its ready_cmd, then `stack down` and `stack up`"
        ),
        None if changed => "what it launched stays recorded: `stack status` shows it, `stack up` again keeps waiting for \
             services still starting (give it a longer timeout if startup needs one), and `stack down` stops them"
            .into(),
        None => "nothing was started; retry, with a longer timeout if this step needs it".into(),
    };
    StackError::new("timed_out", format!("`stack {command}` did not finish within {}s", timeout.as_secs()))
        .hint(hint)
        .details(details)
}

/// `error` with what each started service's declared `ready_cmd` answers now, for the services
/// that do not pass it: its exit status (or that it did not answer) and the last lines it
/// printed. Each runs once, bounded, also when the deadline already passed. Nothing is added
/// when every declared check passes or none is declared.
fn with_readiness(ctx: &Ctx, report: &Report, names: &[String], error: StackError) -> StackError {
    let _grace = crate::process::grace_scope(READINESS_GRACE);
    let Ok(env) = provider_env(ctx, report) else { return error };
    let failing: Vec<Value> = names.iter().filter_map(|name| ready_failure(ctx, report, &env, name)).collect();
    if failing.is_empty() {
        return error;
    }
    error.with_detail(json!({ "readiness": failing }))
}

/// [`with_readiness`], except for a cancelled start: the caller asked that nothing more run.
fn with_readiness_unless_cancelled(ctx: &Ctx, report: &Report, names: &[String], error: StackError) -> StackError {
    if error.code == "cancelled" {
        return error;
    }
    with_readiness(ctx, report, names, error)
}

/// Time a failed or cut-short start may take to run each `ready_cmd` once more for its report.
const READINESS_GRACE: Duration = Duration::from_secs(6);

/// What `name`'s declared `ready_cmd` answers, when it does not succeed (see
/// [`check_ready_cmd`]). `None` for a service that declares none.
fn ready_failure(ctx: &Ctx, report: &Report, env: &IndexMap<String, String>, name: &str) -> Option<Value> {
    let command = report.stack.services.get(name)?.value.ready_cmd.as_deref()?;
    check_ready_cmd(ctx, report, env, name, command).err()
}

/// Run `name`'s `command` (its `ready_cmd`) once. `Ok` only when it ran and exited 0 in time;
/// otherwise its exit status and last lines, or, when it could not be run or read, why
/// (`error`): a check that answered nothing never passes.
fn check_ready_cmd(ctx: &Ctx, report: &Report, env: &IndexMap<String, String>, name: &str, command: &str) -> std::result::Result<(), Value> {
    let out = match run_ready_cmd(ctx, report, env, name, command) {
        Ok(out) => out,
        Err(e) => return Err(json!({ "service": name, "ready_cmd": command, "error": format!("cannot run it through sh: {e}") })),
    };
    if !out.timed_out && out.exit_code == Some(0) {
        return Ok(());
    }
    let tail = |text: &str| -> String {
        let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
        truncate(&lines[lines.len().saturating_sub(3)..].join("\n"), 400)
    };
    Err(json!({
        "service": name,
        "ready_cmd": command,
        "exit_code": out.exit_code,
        "timed_out": out.timed_out,
        "stdout": tail(&out.stdout),
        "stderr": tail(&out.stderr),
    }))
}

/// One line naming the services whose `ready_cmd` fails, from an error [`with_readiness`]
/// annotated (the detail may sit in a nested cause).
fn unready_hint(error: &StackError) -> Option<String> {
    let failing = error.details.iter().find_map(|d| d.get("readiness")?.as_array().cloned())?;
    let parts: Vec<String> = failing
        .iter()
        .map(|f| {
            let service = f["service"].as_str().unwrap_or_default();
            if let Some(error) = f["error"].as_str() {
                return format!("{service}'s ready_cmd could not be run ({error})");
            }
            let how = match f["exit_code"].as_i64() {
                _ if f["timed_out"] == true => "does not answer".to_string(),
                Some(code) => format!("exits {code}"),
                None => "ends by a signal".to_string(),
            };
            let said = f["stderr"].as_str().filter(|s| !s.is_empty()).or(f["stdout"].as_str().filter(|s| !s.is_empty()));
            match said {
                Some(said) => format!("{service}'s ready_cmd {how} ({})", said.lines().last().unwrap_or(said)),
                None => format!("{service}'s ready_cmd {how}"),
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// Verify every service until all pass, or fail with `not_ready` after READY_TIMEOUT (or
/// earlier, at the caller's deadline). `observe` sees each supervisor listing before it is
/// checked. Errors name their step.
fn verify_until_ready(
    ctx: &Ctx,
    report: &Report,
    timings: &'static str,
    mut observe: impl FnMut(&[DaemonStatus]) -> Result<()>,
) -> Verified {
    let window = Instant::now() + READY_TIMEOUT;
    let deadline = crate::process::deadline().map_or(window, |d| d.min(window));
    let timings = Timings::new(timings);
    let names: Vec<String> = report.stack.services.keys().cloned().collect();
    // The environment is the provider's for this configuration: asked once.
    let env = provider_env(ctx, report).map_err(|e| ("verify", e))?;
    let mut seen: Option<Vec<DaemonStatus>> = None;
    // Each service's last real check: a service the supervisor still reports starting is not
    // checked (it cannot pass), one that passed is kept (a service that exits after is caught
    // by `exited_on_start`), the others are checked every pass, and at the deadline each is
    // reported as last seen rather than checked again with no time left.
    let mut last: IndexMap<String, Check> = IndexMap::new();
    let mut pause = Pause::new();
    loop {
        let statuses = listing(ctx, report);
        if seen.as_ref() != Some(&statuses) {
            observe(&statuses).map_err(|e| ("record_observed", e))?;
            seen = Some(statuses.clone());
        }
        exited_on_start(ctx, &statuses, &names).map_err(|e| ("verify", e))?;
        let starting: Vec<String> = statuses.iter().filter(|d| d.status == "starting").map(|d| d.name.clone()).collect();
        let due: Vec<String> = names.iter().filter(|n| !starting.contains(n) && !last.get(*n).is_some_and(|c| c.ready)).cloned().collect();
        if !due.is_empty() && !crate::process::expired() {
            for check in verify_some(&ctx.root, report, &env, &statuses, &timings, Some(&due)) {
                last.insert(check.service.clone(), check);
            }
        }
        for name in &starting {
            last.shift_remove(name);
        }
        if starting.is_empty() && names.iter().all(|n| last.get(n).is_some_and(|c| c.ready)) {
            let checks = names.iter().filter_map(|n| last.shift_remove(n)).collect();
            return Ok((checks, env));
        }
        let now = Instant::now();
        if now >= deadline || crate::process::expired() {
            let checks: Vec<Check> = names.iter().map(|n| last.shift_remove(n).unwrap_or_else(|| not_verified_in_time(report, n))).collect();
            return Err(("verify", not_ready(&checks)));
        }
        sleep(pause.next(!starting.is_empty()).min(deadline - now));
    }
}

/// How long a wait for services sleeps between looks: briefly while they just started, longer
/// once the wait drags on, so a quick start stays quick and a slow one costs little.
struct Pause {
    since: Instant,
    slow: Duration,
}

impl Pause {
    fn new() -> Self {
        Self { since: Instant::now(), slow: Duration::from_millis(25) }
    }

    /// The next pause: 5 ms (while the supervisor reports a service starting) or 25 ms for the
    /// first two seconds, then growing by half each time up to half a second.
    fn next(&mut self, starting: bool) -> Duration {
        if self.since.elapsed() < Duration::from_secs(2) {
            return Duration::from_millis(if starting { 5 } else { 25 });
        }
        self.slow = (self.slow * 3 / 2).min(Duration::from_millis(500));
        self.slow
    }
}

/// The check of a service the deadline came before Stack could verify: the supervisor still
/// reported it starting.
fn not_verified_in_time(report: &Report, name: &str) -> Check {
    Check {
        service: name.to_string(),
        ready: false,
        port: report.ports.get(name).copied(),
        pid: None,
        identity: None,
        reason: Some("not verified before the deadline: the supervisor still reported it starting".into()),
        reason_code: Some(ReasonCode::NotRunning),
        withheld: Vec::new(),
        changed_since_start: Vec::new(),
        watch_incomplete: false,
        checked_at: now(),
        data_dir: None,
        provider_id: None,
    }
}

fn not_ready(checks: &[Check]) -> StackError {
    let failed: Vec<Value> = checks
        .iter()
        .filter(|c| !c.ready)
        .map(|c| json!({ "service": c.service, "reason": c.reason }))
        .collect();
    StackError::new("not_ready", format!("{} service(s) failed verification", failed.len())).details(failed)
}

/// What `restart` did, serialized like [`UpReport`].
#[derive(Debug)]
pub struct RestartReport {
    pub session: Session,
    pub restarted: Vec<String>,
    pub checks: Vec<Check>,
    pub steps: Vec<Value>,
}

impl Serialize for RestartReport {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        StartedView {
            session: SessionView::of(&self.session),
            lease: None,
            restarted: Some(&self.restarted),
            checks: &self.checks,
            steps: compact_steps(&self.steps),
            reaped: &[],
            warnings: &[],
            reassigned_ports: &[],
            init: None,
        }
        .serialize(s)
    }
}

/// Stop and start the named services (every service when none are named) of the running,
/// current session, then verify the whole stack again. Other services keep running. The
/// session keeps its id: the configuration is unchanged, only processes are new.
///
/// Everything happens within `timeout`, as for [`up`]. When it passes, services not named keep
/// running and the session is left incomplete (unverified) with what was launched recorded.
pub fn restart(ctx: &Ctx, services: &[String], timeout: Duration) -> Result<RestartReport> {
    let _deadline = crate::process::deadline_scope(Some(startup_deadline(timeout)?));
    restart_within(ctx, services, false).map_err(|e| startup_timed_out(e, "restart", timeout))
}

/// [`restart`] of exactly the services whose `watch` paths changed after they started (what
/// `status`, `exec` and `run` report as `changed_since_start`). With none changed nothing is
/// restarted, and the result is the stack verified as it runs.
pub fn restart_changed(ctx: &Ctx, timeout: Duration) -> Result<RestartReport> {
    let _deadline = crate::process::deadline_scope(Some(startup_deadline(timeout)?));
    restart_within(ctx, &[], true).map_err(|e| startup_timed_out(e, "restart", timeout))
}

fn restart_within(ctx: &Ctx, services: &[String], changed_only: bool) -> Result<RestartReport> {
    let mut steps = Steps::default();
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?.ok_or_else(|| {
        StackError::new("no_session", "no session for this project").hint("run `stack up`")
    })?;
    settle_executions(ctx, &mut session)?;
    if has_active_executions(&session) {
        return Err(busy(&session, "restart"));
    }
    let report = ctx.compile(true)?;
    refuse_root(&report)?;
    mise::trust_once(&ctx.state, &ctx.root)?;
    if legacy_provider(Some(&session)).is_some() {
        return Err(StackError::new("session_stale", "these services were started by an older Stack under Pitchfork")
            .hint("run `stack up`; it stops them there and starts them under Stack's own supervisor, keeping their data"));
    }
    if session.launching {
        return Err(StackError::new("session_stale", "the last start or restart of this session did not finish verifying")
            .hint("run `stack up`; it starts and verifies every service"));
    }
    if !same_config(&session, ctx, &report) {
        return Err(StackError::new(
            "session_stale",
            "the running services were started from a different configuration",
        )
        .hint("run `stack up`; it restarts and verifies every service"));
    }
    let mut names: Vec<String> = Vec::new();
    for name in services {
        if !report.stack.services.contains_key(name) {
            return Err(unknown_service(&report, name));
        }
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    if changed_only {
        names = report
            .stack
            .services
            .iter()
            .filter(|(_, e)| !e.value.watch.is_empty())
            .filter_map(|(name, e)| {
                let started = session.services.get(name)?.started_at_ms?;
                (!changed_since(&ctx.root, &e.value.watch, started).0.is_empty()).then(|| name.clone())
            })
            .collect();
        if names.is_empty() {
            let (env, statuses) = (provider_env(ctx, &report)?, listing(ctx, &report));
            let checks = verify_session(ctx, &report, &env, &statuses, Some(&session), &Timings::new("restart"));
            steps.ok("changed", json!([]));
            return Ok(RestartReport { session, restarted: Vec::new(), checks, steps: steps.0 });
        }
        steps.ok("changed", json!(names));
    }
    if names.is_empty() {
        names = report.stack.services.keys().cloned().collect();
    }
    if names.is_empty() {
        return Err(StackError::new("unknown_service", "this project defines no services"));
    }
    // Services that depend on one being restarted restart after it, so they reconnect.
    let dependents = dependents_of(&report.stack.services, &names);
    if !dependents.is_empty() {
        steps.ok("dependents", json!(dependents));
        names.extend(dependents);
    }

    // What they depend on and are not restarted with must already run, or nothing is touched.
    if let Err(e) = prerequisites_ready(ctx, &report, &names, &names) {
        return Err(steps.fail("dependencies", e, false));
    }

    // Confirm the old processes are gone before starting new ones on the same ports.
    steps.begin("stop");
    if let Err(e) = stop_and_wait(ctx, &names, Some(&session)) {
        return Err(steps.fail("stop", e, true));
    }
    steps.ok("stop", json!(names));
    // The supervisor stamps output with milliseconds, so `logs --since-start` keeps only what
    // the new processes wrote.
    let started_at = now_ms();

    // Like `up`, an incomplete launch is recorded before anything starts, and every replacement
    // the supervisor reports is recorded as it is observed: if starting or verification fails,
    // `down`, `up` and GC still know what this restart may have launched.
    for name in &names {
        if let Some(record) = session.services.get_mut(name) {
            record.pid = None;
            record.started_at_ms = None;
        }
    }
    session.launching = true;
    session.provider = Some(ProviderRecord::native(supervisor_dir(ctx)));
    save(ctx, &session).map_err(|e| steps.clone().fail("record_launch", e, true))?;
    steps.begin("start");
    if let Err(start_error) = start_in_order(ctx, &report, &names) {
        let observed = record_after_failed_start(ctx, &report);
        if let Err(error) = observed {
            return Err(steps.fail("record_partial_start", error.with_detail(json!({ "start_error": start_error })), true));
        }
        return Err(steps.fail("start", with_readiness_unless_cancelled(ctx, &report, &names, start_error), true));
    }
    steps.ok("start", json!(names));
    steps.begin("verify");
    let (checks, _) = verify_launch(ctx, &report, "restart_verify").map_err(|(step, e)| steps.clone().fail(step, with_readiness(ctx, &report, &names, e), true))?;
    steps.ok("verify", json!(checks.iter().map(|c| (c.service.clone(), c.identity)).collect::<IndexMap<_, _>>()));
    bind_data(ctx, &checks);

    for check in &checks {
        // Untouched services keep their start time while they are the same process.
        let started = match session.services.get(&check.service) {
            Some(r) if !names.contains(&check.service) && r.pid.is_some() && r.pid == check.pid => r.started_at_ms,
            _ => Some(started_at),
        };
        session.services.insert(check.service.clone(), record_of(check, started));
    }
    session.launching = false;
    if let Some(lease) = session.lease.as_mut() {
        lease.renewed_at = now();
    }
    save(ctx, &session).map_err(|e| steps.clone().fail("record_session", e, true))?;
    Ok(RestartReport { session, restarted: names, checks, steps: steps.0 })
}

/// What [`with_stopped`] did around its operation.
#[derive(Debug, Serialize)]
pub struct StoppedReport {
    pub steps: Vec<Value>,
    /// Services that were running, stopped for the operation (with those that depend on them)
    /// and started and verified again.
    pub restarted: Vec<String>,
    pub checks: Vec<Check>,
    /// What the operation returned.
    pub result: Value,
}

/// Run `operation` with the named services confirmed stopped, under the project lock, then
/// start and verify again those that were running (for snapshots, which copy data
/// directories). Stopping and starting are `restart`'s: services that depend on a stopped one
/// are stopped first and started after it, each service's `setup` runs before it starts, and
/// the supervisor this environment names is the one addressed. `operation` gets the compiled
/// report and the supervisor listing taken once they stopped. Services that run must belong to
/// the current, verified session; a stale or incomplete one is refused (`session_stale`). If
/// the operation fails after stopping, the services are started again before the error is
/// returned.
pub fn with_stopped(
    ctx: &Ctx,
    names: &[String],
    operation: impl FnOnce(&Report, &[DaemonStatus]) -> Result<Value>,
) -> Result<StoppedReport> {
    let _deadline = crate::process::deadline_scope(Some(startup_deadline(DEFAULT_STARTUP_TIMEOUT)?));
    let mut steps = Steps::default();
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?;
    if let Some(s) = session.as_mut() {
        settle_executions(ctx, s)?;
        if has_active_executions(s) {
            return Err(busy(s, "snapshot"));
        }
    }
    let report = ctx.compile(true)?;
    for name in names {
        if !report.stack.services.contains_key(name) {
            return Err(unknown_service(&report, name));
        }
    }
    refuse_root(&report)?;
    mise::trust_once(&ctx.state, &ctx.root)?;
    if legacy_provider(session.as_ref()).is_some() {
        return Err(StackError::new("session_stale", "these services were started by an older Stack under Pitchfork")
            .hint("run `stack up` (or `stack down`) first, then retry"));
    }
    let statuses = listing(ctx, &report);
    let live = |n: &String| {
        statuses.iter().any(|d| d.name == *n && may_run(d))
            || session.as_ref().and_then(|s| s.services.get(n)).is_some_and(|r| r.pid.is_some_and(pid_alive))
    };
    let mut stopping: Vec<String> = names.iter().filter(|n| live(n)).cloned().collect();
    if !stopping.is_empty() {
        let current = session.as_ref().is_some_and(|s| !s.launching && same_config(s, ctx, &report));
        if !current {
            return Err(StackError::new(
                "session_stale",
                format!("{} running from a configuration or launch this checkout has not verified", stopping.join(", ")),
            )
            .hint("run `stack up` (or `stack down`) first, then retry"));
        }
        // What holds connections to a stopped service is stopped with it and started after it.
        let dependents: Vec<String> = dependents_of(&report.stack.services, &stopping).into_iter().filter(|n| live(n)).collect();
        stopping.extend(dependents);
        steps.begin("stop");
        if let Err(e) = stop_and_wait(ctx, &stopping, session.as_ref()) {
            return Err(steps.fail("stop", e, true));
        }
        steps.ok("stop", json!(stopping));
    }
    // The supervisor must agree that nothing named still runs before any data is touched.
    let stopped = listing(ctx, &report);
    let still: Vec<&str> = names
        .iter()
        .filter(|n| stopped.iter().any(|d| d.name == **n && may_run(d)))
        .map(String::as_str)
        .collect();
    if !still.is_empty() {
        let err = StackError::new("stop_unconfirmed", format!("the supervisor still reports {} running; no data was touched", still.join(", ")))
            .hint("run `stack down`, then retry");
        return Err(steps.fail("confirm_stopped", err, !stopping.is_empty()));
    }
    // What a dead supervisor left that may still run, or cannot be shown not to, keeps the data
    // as it is.
    let unresolved = supervisor::unresolved(&supervisor_dir(ctx));
    if !unresolved.is_empty() {
        let err = StackError::new("stop_unconfirmed", "processes of this checkout's services may still run; no data was touched")
            .hint("run `stack down`, which stops what it can show is this checkout's and names the rest, then retry")
            .details(unresolved);
        return Err(steps.fail("confirm_stopped", err, !stopping.is_empty()));
    }
    let outcome = operation(&report, &stopped);
    let result = match &outcome {
        Ok(v) => {
            steps.ok("copy", v.clone());
            v.clone()
        }
        Err(e) => {
            steps.0.push(json!({ "step": "copy", "status": "failed", "code": e.code }));
            Value::Null
        }
    };
    let mut checks = Vec::new();
    if let Some(mut session) = session.filter(|_| !stopping.is_empty()) {
        let started_at = now_ms();
        for name in &stopping {
            if let Some(record) = session.services.get_mut(name) {
                record.pid = None;
                record.started_at_ms = None;
            }
        }
        session.launching = true;
        session.provider = Some(ProviderRecord::native(supervisor_dir(ctx)));
        let restarted = save(ctx, &session)
            .map_err(|e| ("record_launch", e))
            .and_then(|()| {
                start_in_order(ctx, &report, &stopping).map_err(|e| {
                    let _ = record_after_failed_start(ctx, &report);
                    ("start", with_readiness_unless_cancelled(ctx, &report, &stopping, e))
                })
            })
            .and_then(|()| verify_launch(ctx, &report, "snapshot_verify").map(|(checks, _)| checks));
        match restarted {
            Ok(verified) => {
                steps.ok("start", json!(stopping));
                steps.ok("verify", json!(verified.iter().map(|c| (c.service.clone(), c.identity)).collect::<IndexMap<_, _>>()));
                bind_data(ctx, &verified);
                for check in &verified {
                    let started = match session.services.get(&check.service) {
                        Some(r) if !stopping.contains(&check.service) && r.pid.is_some() && r.pid == check.pid => r.started_at_ms,
                        _ => Some(started_at),
                    };
                    session.services.insert(check.service.clone(), record_of(check, started));
                }
                session.launching = false;
                save(ctx, &session).map_err(|e| steps.clone().fail("record_session", e, true))?;
                checks = verified;
            }
            Err((step, e)) => {
                let e = match outcome {
                    Err(first) => first.with_detail(json!({ "restart_error": e })),
                    Ok(_) => e,
                };
                return Err(steps.fail(step, e, true));
            }
        }
    }
    match outcome {
        Ok(_) => Ok(StoppedReport { steps: steps.0, restarted: stopping, checks, result }),
        Err(e) => {
            let changed = !stopping.is_empty();
            // The failed copy step is already recorded; attach the progress record.
            Err(e.with_detail(json!({ "steps": steps.0, "retry_safe": true, "changed": changed })))
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub steps: Vec<Value>,
    pub ports: IndexMap<String, u16>,
    pub versions: Vec<project::VersionReport>,
    /// Problems that did not stop installation (the `skills` step).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Install every locked tool and service binary without starting anything or recording a
/// session: the compile, trust, socket preflight and install steps of `up`, and nothing after.
/// Locked like `up`: a missing or stale pin is `lock_outdated`, never resolved here.
pub fn install(ctx: &Ctx) -> Result<InstallReport> {
    let mut steps = Steps::default();
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    steps.begin("compile");
    let report = match ctx.compile_to_install() {
        Ok(r) => r,
        Err((step, e)) => return Err(steps.fail(step, e, false)),
    };
    steps.ok("compile", json!({ "ports": report.ports }));
    let plan = match prepare_install(ctx, &report) {
        Ok(plan) => plan,
        Err(e) => return Err(steps.fail("install", e, false)),
    };
    let recorded_trust = match mise::trust_once(&ctx.state, &ctx.root) {
        Ok(skipped) => skipped,
        Err(e) => return Err(steps.fail("install", e, false)),
    };
    steps.begin("install");
    match install_partitioned(ctx, &plan, &report, recorded_trust) {
        Ok(detail) => steps.ok("install", detail),
        Err(e) => return Err(steps.fail("install", e, false)),
    }
    let warnings = steps.skills(ctx, &report);
    Ok(InstallReport { steps: steps.0, ports: report.ports, versions: report.versions, warnings })
}

/// What the install step will do, decided before anything is installed.
struct InstallPlan {
    artifacts: artifacts::PlatformSummary,
    partition: artifacts::Partition,
    /// A service preset whose tool stack does not know: plain `mise install` of everything
    /// left, as before artifact locking.
    everything: bool,
}

/// Stated with every install: what `verified` means for releases already on the machine.
const INSTALL_BOUNDARY: &str = "mise checks the recorded checksum (and packslip signer) of artifacts it downloads; a release already installed on this machine is reported installed without being checked again";

/// Before anything installs: the artifact policy met on this platform, and
/// `.config/mise/mise.lock` rendered from stack.lock. The provider release was checked before
/// the compile wrote anything (`Ctx::compile_to_install`).
fn prepare_install(ctx: &Ctx, report: &Report) -> Result<InstallPlan> {
    let plan = install_plan(report)?;
    artifacts::write_rendered(&ctx.root, report.lock.as_ref())?;
    Ok(plan)
}

/// The artifact policy met on this platform, and how each tool installs here. Nothing is
/// written.
fn install_plan(report: &Report) -> Result<InstallPlan> {
    let platform = artifacts::current_platform();
    let policy = &report.artifact_policy;
    let pins = report.pins();
    let lock = report.provider_lock();
    // The locked compile already refused an unlisted platform and unchecked listed ones; this
    // platform's coverage is checked again here because it is what the install relies on.
    if policy.required() {
        policy.check_runtime_platform()?;
        let unchecked = artifacts::unchecked(lock, &pins, std::slice::from_ref(&platform), &Default::default());
        if !unchecked.is_empty() {
            return Err(artifacts::unlocked_error(unchecked, false));
        }
    }
    let mut partition = artifacts::partition(lock, &pins, &platform);
    // Requests that name no release are not pins; they install as they always did.
    for (name, e) in &report.stack.tools {
        if mise::unversioned(&e.value.version) && !partition.plain.contains(name) && !partition.locked.contains(name) {
            partition.plain.push(name.clone());
        }
    }
    let everything = report.stack.services.values().any(|e| e.value.preset.as_deref().is_some_and(|p| mise::preset_tool(p).is_none()));
    Ok(InstallPlan { artifacts: artifacts::summary(lock, &pins, &platform), partition, everything })
}

/// Whether every tool the install would name is an exact pin of stack.lock that mise lists as
/// installed, asked now with one `mise ls --json` naming exactly those pins and their options.
/// `mise install` checks nothing more of a release already installed (see
/// [`INSTALL_BOUNDARY`]), so a repeated `up` skips it and saves a provider start per partition.
/// Anything else, including a question mise could not answer, installs as before, so nothing a
/// download goes through (checksum, signer, platform variant) is skipped for a missing release.
fn already_installed(ctx: &Ctx, plan: &InstallPlan, report: &Report) -> bool {
    let named = || plan.partition.locked.iter().chain(&plan.partition.plain);
    // A request that names no release (`system`, `path:`, `ref:`) is not a pin: mise decides.
    let unpinned = report.stack.tools.values().any(|e| mise::unversioned(&e.value.version));
    if plan.everything || unpinned || named().next().is_none() {
        return false;
    }
    let pins = provider_pins(report);
    // Answered from Stack's kept answer when it is current (see `mise::provide`), which holds
    // only answers where every pin was installed and whose install directories are unchanged.
    named().all(|tool| pins.iter().any(|pin| &pin.tool == tool))
        && mise::provide_missing(&ctx.state, &ctx.cache, &ctx.root, &pins).is_ok_and(|missing| missing.is_empty())
}

/// `mise install --locked` for pins whose coverage here is verified or exempt, then plain
/// `mise install` for the rest; an empty partition is skipped, and so is the whole install
/// when every pin is already installed (see [`already_installed`]).
///
/// `recorded_trust`: the configuration's trust was taken from Stack's record rather than asked
/// of mise (see [`mise::trust_once`]); an install then trusts it again first, in case mise's own
/// record is gone.
fn install_partitioned(ctx: &Ctx, plan: &InstallPlan, report: &Report, recorded_trust: bool) -> Result<Value> {
    if already_installed(ctx, plan, report) {
        return Ok(json!({
            "artifacts": plan.artifacts,
            "locked": plan.partition.locked,
            "plain": plan.partition.plain,
            "boundary": INSTALL_BOUNDARY,
            "skipped": "every pinned release is already installed",
        }));
    }
    if recorded_trust {
        mise::trust(&ctx.root)?;
    }
    // Whatever it installs changes what mise answers; the kept answers would not match anyway.
    crate::provider::env_cache::forget(&ctx.state, &ctx.root);
    if !plan.partition.locked.is_empty() {
        mise::install_tools(&ctx.root, &plan.partition.locked, true)?;
    }
    if !plan.partition.plain.is_empty() {
        mise::install_tools(&ctx.root, &plan.partition.plain, false)?;
    }
    if plan.everything {
        mise::install_tools(&ctx.root, &[], false)?;
    }
    Ok(json!({
        "artifacts": plan.artifacts,
        "locked": plan.partition.locked,
        "plain": plan.partition.plain,
        "boundary": INSTALL_BOUNDARY,
    }))
}

#[derive(Debug, Serialize)]
pub struct LogsReport {
    pub service: String,
    /// The last lines the supervisor kept for this service, oldest first.
    pub lines: Vec<String>,
    /// The supervisor returned more than stack's capture limit; `lines` holds the tail.
    pub truncated: bool,
    /// When stack last started the service's current process, if recorded. Lines before it
    /// came from an earlier process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// Only lines since `started_at` were requested.
    pub since_start: bool,
}

/// The last `tail` lines of a service's output, as the supervisor stored them. Bounded in
/// time and size; never follows. With `since_start`, only output of the current process.
pub fn logs(ctx: &Ctx, service: &str, tail: usize, since_start: bool) -> Result<LogsReport> {
    logs_since(ctx, service, tail, since_start, None)
}

/// Every service the project defines, in order.
pub fn service_names(ctx: &Ctx) -> Result<Vec<String>> {
    Ok(ctx.compile(false)?.stack.services.keys().cloned().collect())
}

/// [`logs`], also keeping only output written at or after `since` (Unix seconds) when given.
pub fn logs_since(ctx: &Ctx, service: &str, tail: usize, since_start: bool, since: Option<u64>) -> Result<LogsReport> {
    let report = ctx.compile(false)?;
    if !report.stack.services.contains_key(service) {
        return Err(unknown_service(&report, service));
    }
    let (session, started_ms) = {
        let _guard = project_lock(&ctx.state, &ctx.root)?;
        let session = load(ctx)?;
        let started_ms = session.as_ref().and_then(|s| s.services.get(service)).and_then(|r| r.started_at_ms);
        (session, started_ms)
    };
    let started_at = started_ms.map(|ms| ms / 1000);
    let dir = supervisor_dir(ctx);
    let legacy = legacy_provider(session.as_ref()).cloned();
    if session.is_none() && !supervisor::log_path(&dir, service).exists() {
        return Err(StackError::new("logs_failed", format!("no launch record for {service} in this checkout"))
            .hint("run `stack up` first"));
    }
    if since_start && started_ms.is_none() {
        return Err(StackError::new("logs_failed", format!("no recorded start time for {service}"))
            .hint("sessions started before stack 0.1.18 lack one; `stack restart` records it"));
    }
    let (lines, truncated) = match legacy {
        // A session an older Stack started: Pitchfork kept its output, stamped in whole seconds.
        Some(provider) => {
            let since = match (since_start, started_at) {
                (true, Some(at)) => Some(since.map_or(at, |s| s.max(at))),
                _ => since,
            };
            let id = session.as_ref().and_then(|s| s.services.get(service)).and_then(|r| r.provider_id.clone()).ok_or_else(|| {
                StackError::new("logs_failed", format!("no supervisor id was recorded for {service}")).hint("run `stack up`")
            })?;
            let out = mise::legacy_logs(provider.legacy().expect("legacy"), &provider.state_dir, &id, tail, since)?;
            (out.stdout.lines().map(str::to_string).collect::<Vec<_>>(), out.stdout_truncated)
        }
        // Stack's supervisor stamps each line in milliseconds: since the start is exact.
        None => {
            let since_ms = match (since_start, started_ms) {
                (true, Some(at)) => Some(since.map_or(at, |s| (s * 1000).max(at))),
                _ => since.map(|s| s * 1000),
            };
            supervisor::logs(&dir, service, tail, since_ms).map_err(|e| StackError::new("logs_failed", format!("cannot read {service}'s output: {e}")))?
        }
    };
    let skip = lines.len().saturating_sub(tail);
    Ok(LogsReport {
        service: service.to_string(),
        lines: lines[skip..].to_vec(),
        truncated,
        started_at,
        since_start,
    })
}

/// `unknown_service`, naming the services the project does define.
fn unknown_service(report: &Report, name: &str) -> StackError {
    let known: Vec<&str> = report.stack.services.keys().map(String::as_str).collect();
    let listed = name.contains(',') && name.split(',').all(|n| known.contains(&n.trim()));
    StackError::new("unknown_service", format!("no service named '{name}'")).hint(if known.is_empty() {
        "this project defines no services".to_string()
    } else if listed {
        // `--require postgres,redis`: one name per flag.
        let flags: Vec<String> = name.split(',').map(|n| format!("--require {}", n.trim())).collect();
        format!("name one service per flag: {} (services: {})", flags.join(" "), known.join(", "))
    } else {
        format!("services: {}", known.join(", "))
    })
}

/// Device and inode of a directory, identifying it beyond its path.
pub fn dir_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

pub(crate) fn dir_created(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.created().ok()
}

fn project_replaced(session: &Session, path: &Path) -> bool {
    if let (Some(recorded), Some(current)) = (session.project_dir_id, dir_id(path)) {
        if recorded != current {
            return true;
        }
    }
    // Once recorded, losing the birth time also prevents adoption of the old session.
    session.project_dir_created.is_some_and(|recorded| Some(recorded) != dir_created(path))
}

fn new_session_id(ctx: &Ctx) -> String {
    sha256_hex(format!("{}{:?}{}", ctx.root.display(), Instant::now(), std::process::id()).as_bytes())[..12]
        .to_string()
}

/// Every service is the same process the previous record verified.
fn checks_match(previous: &Session, checks: &[Check]) -> bool {
    previous.services.len() == checks.len()
        && checks.iter().all(|c| {
            previous
                .services
                .get(&c.service)
                .is_some_and(|r| r.pid == c.pid && Some(r.port) == c.port)
        })
}

/// The session record of a verified check.
fn record_of(check: &Check, started_at_ms: Option<u64>) -> ServiceRecord {
    ServiceRecord {
        port: check.port.unwrap_or_default(),
        pid: check.pid,
        data_dir: check.data_dir.clone(),
        identity: check.identity.unwrap_or(Identity::Liveness),
        verified_at: check.checked_at,
        started_at_ms,
        provider_id: check.provider_id.clone(),
    }
}

#[derive(Debug, Serialize)]
pub struct DownReport {
    pub stopped: Vec<Value>,
    pub confirmed: bool,
    /// Reserved ports a program stack does not own is listening on. Not a failure of `down`:
    /// nothing stack started is behind them, and `up` refuses to start over them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<PortConflict>,
    /// What happened to the supervisor that ran this checkout's services: `{stopped, reason}`
    /// or `{stopped, state_dir}` (see [`stop_owned_supervisor`]). Absent for a checkout without
    /// services.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<Value>,
    /// What `down --purge` removed and released (see [`purge_locked`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge: Option<PurgeReport>,
    /// Session records that could not be read, removed once what the supervisor recorded was
    /// confirmed stopped (see [`session_record`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_unreadable: Vec<PathBuf>,
    /// For an unreadable record cleared with no supervisor record: what was checked, and what
    /// could not be (see [`TOOLS_ONLY_CHECKED`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_checked: Option<String>,
    /// Unrecorded command registry entries `--forget-unrecorded` removed.
    #[serde(skip_serializing_if = "is_zero")]
    pub forgotten_unrecorded: usize,
    /// What did not keep `down` from confirming but needs attention (a marker of an earlier
    /// unconfirmed `down` that could not be removed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What `down --purge` did with this checkout's recorded data, ports and identity tokens.
#[derive(Debug, Serialize)]
pub struct PurgeReport {
    /// Each data directory `up` recorded for this checkout, and what became of it.
    pub data: Vec<PurgedData>,
    /// Every recorded data directory was removed, or found no longer to be this checkout's.
    /// When false, `data` says what was kept and why.
    pub complete: bool,
    pub released_ports: Vec<Value>,
    pub released_identities: usize,
}

#[derive(Debug, Serialize)]
pub struct PurgedData {
    pub service: String,
    pub dir: PathBuf,
    pub outcome: String,
    /// Kept because Stack could not confirm it is safe to remove; `outcome` says why.
    pub kept: bool,
}

/// A port reserved for one of this checkout's services that a foreign program holds.
#[derive(Debug, Clone, Serialize)]
pub struct PortConflict {
    pub service: String,
    pub port: u16,
    /// The project pinned this port in `[override.services]`; stack cannot move it.
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<ports::Holder>,
}

/// Stop the project's services. Succeeds only once their processes are gone and ports closed.
/// Then stops the supervisor that ran them when Stack started it and nothing else uses it
/// (see [`stop_owned_supervisor`]).
pub fn down(ctx: &Ctx) -> Result<DownReport> {
    down_with(ctx, true)
}

/// [`down`]; with `stop_supervisor` false the supervisor is kept whoever started it.
pub fn down_with(ctx: &Ctx, stop_supervisor: bool) -> Result<DownReport> {
    down_purging(ctx, stop_supervisor, false)
}

/// [`down_with`]; with `purge`, once every service is confirmed stopped, also remove the data
/// directories `up` recorded for this checkout and release its ports and identity tokens (see
/// [`purge_locked`]), under the same project lock.
pub fn down_purging(ctx: &Ctx, stop_supervisor: bool, purge: bool) -> Result<DownReport> {
    down_forgetting(ctx, stop_supervisor, purge, false)
}

/// [`down_purging`]; with `forget_unrecorded`, first remove the checkout's command registry
/// entries whose process was never recorded and whose Stack process is gone (see
/// `crate::commands`), which otherwise keep every `down` unconfirmed. Nothing else in the
/// registry is touched.
///
/// A `down` that ends without confirming (for any reason, once it holds the lock) leaves a
/// marker for the checkout (see [`unconfirmed_marker`]), which only `status` reads: without a
/// session it keeps the checkout started until a `down` confirms, which removes it.
pub fn down_forgetting(ctx: &Ctx, stop_supervisor: bool, purge: bool, forget_unrecorded: bool) -> Result<DownReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let result = (|| {
        let forgotten = if forget_unrecorded { commands::forget_unrecorded(&ctx.state, &ctx.root)? } else { 0 };
        let mut report = down_unforgetting(ctx, stop_supervisor, purge)?;
        report.forgotten_unrecorded = forgotten;
        Ok(report)
    })();
    match result {
        Ok(mut report) => {
            // The stop is confirmed (and any purge done) whatever becomes of the marker.
            if let Err(e) = clear_unconfirmed(ctx) {
                let path = unconfirmed_marker(ctx).map_or_else(String::new, |p| format!(" {}", p.display()));
                report.warnings.push(format!(
                    "the marker of an earlier unconfirmed `stack down`{path} could not be removed ({}); `stack status` keeps reporting that `down` as unconfirmed until it is: delete it",
                    e.message
                ));
            }
            Ok(report)
        }
        Err(mut e) => {
            if let Err(why) = mark_unconfirmed(ctx, &e) {
                e.message.push_str(&format!("; this unconfirmed `stack down` could not be recorded, so `stack status` may not show it ({})", why.message));
                e.details.push(json!({ "kind": "marker", "error": why.message }));
            }
            Err(e)
        }
    }
}

/// Where a `down` that did not confirm leaves its marker for the checkout at `ctx.root` (keyed
/// by the directory's identity, as the command registry is), when that identity can be computed.
fn unconfirmed_marker(ctx: &Ctx) -> Option<PathBuf> {
    crate::checkout::identity(&ctx.root).map(|id| ctx.state.join("unconfirmed").join(format!("{id}.json")))
}

/// The marker a `down` that did not confirm leaves: when, and its error. Nothing but `status`
/// reads it.
#[derive(Debug, Serialize, Deserialize)]
struct UnconfirmedDown {
    at_ms: u64,
    code: String,
    message: String,
}

fn mark_unconfirmed(ctx: &Ctx, e: &StackError) -> Result<()> {
    let path = unconfirmed_marker(ctx).ok_or_else(|| StackError::new("state_invalid", "this directory's identity cannot be computed"))?;
    write_json(&path, &UnconfirmedDown { at_ms: now_ms(), code: e.code.to_string(), message: e.message.clone() })
}

fn clear_unconfirmed(ctx: &Ctx) -> Result<()> {
    match unconfirmed_marker(ctx) {
        Some(path) => remove_if_exists(&path),
        None => Ok(()),
    }
}

/// What `status` says of the marker a `down` that did not confirm left, if there is one (or one
/// that cannot be read).
fn unconfirmed_down(ctx: &Ctx) -> Option<String> {
    let path = unconfirmed_marker(ctx)?;
    let unknown = |why: String| format!("cannot tell whether the last `stack down` here was confirmed ({}: {why}); run `stack down` again", path.display());
    match fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(unknown(e.to_string())),
        Ok(bytes) => Some(match serde_json::from_slice::<UnconfirmedDown>(&bytes) {
            Ok(m) => format!(
                "the last `stack down` here ({}s ago) was not confirmed ({}: {}); what it reported may still run: run `stack down` again",
                now_ms().saturating_sub(m.at_ms) / 1000,
                m.code,
                m.message
            ),
            Err(e) => unknown(e.to_string()),
        }),
    }
}

/// [`down_forgetting`] under the project lock, without forgetting anything.
fn down_unforgetting(ctx: &Ctx, stop_supervisor: bool, purge: bool) -> Result<DownReport> {
    // Both copies are read once, under the lock: what decides that one is unreadable is also
    // what any removal is compared with.
    let copies = [RecordCopy::capture(&ctx.index_file()), RecordCopy::capture(&ctx.session_file())];
    if copies.iter().any(RecordCopy::unreadable) {
        return recover_unreadable(ctx, &copies, purge);
    }
    let session = load(ctx)?;
    // As in `down_locked`: a checkout with neither owns no services, and has no supervisor.
    let owns_services = !ports::lookup(&ctx.state, &ctx.root)?.is_empty() || session.as_ref().is_some_and(|s| !s.services.is_empty());
    let recorded = session.as_ref().and_then(|s| s.provider.clone());
    // Stack processes an older Stack recorded as running commands, known from the session only.
    let executing: Vec<u32> = session.as_ref().map(|s| s.active_executions.values().copied().collect()).unwrap_or_default();
    // One decision per process on what is a command, for stopping and for reporting alike.
    let exempt = commands::Exempt::load(&ctx.state, Some(&ctx.root));
    let mut report = down_stopping(ctx, session, None, &exempt)?;
    // Not confirmed while a command Stack started for this checkout still runs, or that cannot
    // be known (and nothing purged): the services are stopped all the same.
    let running = commands::after_down(&ctx.state, &ctx.root, &executing, &exempt);
    if purge && matches!(&running, Ok(r) if r.is_empty()) {
        report.purge = Some(purge_locked(ctx)?);
    }
    // A Pitchfork supervisor an earlier `up` moved this checkout's services off, but could not
    // stop then, is tried again.
    let kept = read_json::<Option<ProviderRecord>>(&kept_pitchfork(ctx)).ok().flatten();
    if owns_services || recorded.is_some() || kept.is_some() {
        let mut outcome = match recorded.filter(|p| p.legacy().is_some()) {
            // Pitchfork, for a session an older Stack started under it.
            Some(legacy) if stop_supervisor => stop_owned_supervisor(ctx, Some(legacy)),
            Some(_) => json!({ "stopped": false, "reason": "asked to keep it" }),
            // Stack's own supervisor exits with the last of its services.
            None => json!({ "stopped": true, "state_dir": supervisor_dir(ctx) }),
        };
        if let Some(legacy) = kept.filter(|_| stop_supervisor) {
            let pitchfork = stop_owned_supervisor(ctx, Some(legacy.clone()));
            pitchfork_outcome(ctx, &legacy, &pitchfork, &mut Vec::new());
            outcome["pitchfork"] = pitchfork;
        }
        report.supervisor = Some(outcome);
    }
    commands::confirm(report.stopped.len(), running)?;
    Ok(report)
}

/// Where the Pitchfork supervisor a migrating `up` could not stop is remembered for the
/// checkout, for the next `down` to try again.
fn kept_pitchfork(ctx: &Ctx) -> PathBuf {
    ctx.state.join("pitchfork-kept").join(format!("{}.json", project_key(&ctx.root)))
}

/// After trying to stop the Pitchfork supervisor of `legacy`: remember it for the next `down`
/// and add the reason to `warnings` when it was kept, forget it when it stopped.
fn pitchfork_outcome(ctx: &Ctx, legacy: &ProviderRecord, outcome: &Value, warnings: &mut Vec<String>) {
    if outcome["stopped"] == true {
        let _ = fs::remove_file(kept_pitchfork(ctx));
        return;
    }
    let reason = outcome["reason"].as_str().unwrap_or("no reason given");
    warnings.push(format!(
        "the Pitchfork supervisor in {} that ran these services before was kept ({reason}); `stack down` tries again, or stop it yourself once nothing uses it",
        legacy.state_dir.display()
    ));
    let _ = write_json(&kept_pitchfork(ctx), &Some(legacy.clone()));
}

/// How long `down` waits for other Stack commands to finish their requests to the supervisor
/// before it keeps the supervisor instead.
const SUPERVISOR_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Pitchfork's id for its supervisor, under which `pitchfork status` reports its PID.
const SUPERVISOR_ID: &str = "global/pitchfork";

/// Stop the supervisor that ran this checkout's services (the one its session recorded, else
/// the one its configuration names now), once they are confirmed gone, only when all of these
/// hold, each checked with the requests to it of every other Stack command, whatever its Stack
/// state, shut out (see [`using_supervisor`]):
///
/// - it runs, and Stack started it: the process Pitchfork names as its supervisor carries this
///   Stack state's owner mark (see [`supervisor_owner`]). One already running when Stack first
///   used it, started by anything else or by another Stack state, or whose environment cannot
///   be read, is not Stack's;
/// - no other checkout's session in this Stack state records the same supervisor, by its
///   resolved state directory, or records services without naming their supervisor, and every
///   session record can be read (another Stack state's sessions are seen through their daemons);
/// - no daemon it tracks (Stack's or anyone's) is anything but stopped, failed or errored, has
///   a live process, or has a schedule, which only a running supervisor keeps (see
///   [`daemons_in_use`]).
///
/// What happened, and why not, is the result; nothing here fails `down`. A supervisor that is
/// not running is not started, and is reported as stopped.
fn stop_owned_supervisor(ctx: &Ctx, recorded: Option<ProviderRecord>) -> Value {
    let kept = |reason: String| json!({ "stopped": false, "reason": reason });
    let Some((provider, bin)) = recorded.and_then(|p| Some((p.clone(), p.legacy()?.to_path_buf()))) else {
        return kept("no Pitchfork supervisor is recorded for this checkout".into());
    };
    let lock = supervisor_lock(&provider.state_dir);
    let alone = match SharedLock::exclusive_within(&lock, SUPERVISOR_LOCK_WAIT) {
        Ok(Some(guard)) => guard,
        Ok(None) => return kept("another Stack command is starting or stopping services through this supervisor".into()),
        Err(e) => return kept(format!("cannot lock {}: {}", lock.display(), e.message)),
    };
    // Asked first: listing daemons would start a supervisor that is not running.
    match mise::supervisor_running(&bin, &provider.state_dir) {
        Ok(false) => return json!({ "stopped": true, "reason": "it was not running" }),
        Ok(true) => {}
        Err(why) => return kept(why),
    }
    let (supervisor, mark) = match owned_supervisor(ctx, &bin, &provider) {
        Ok(found) => found,
        Err(why) => return kept(why),
    };
    if let Some(why) = supervisor_in_use(ctx, &bin, &provider) {
        return kept(why);
    }
    if supervisor.current().is_none() {
        return kept(format!("supervisor pid {} changed while Stack checked it", supervisor.pid));
    }
    // Replaced, the lock no longer shuts out a command that opened it since.
    if !alone.intact() {
        return kept(format!("{} was removed or replaced while Stack checked the supervisor", lock.display()));
    }
    match mise::stop_supervisor(&bin, &provider.state_dir) {
        Ok(true) => {}
        Ok(false) => return kept("it still answers after `pitchfork supervisor stop`".into()),
        Err(why) => return kept(why),
    }
    let left = supervisor_left(&supervisor, &mark);
    if !left.is_empty() {
        let pids: Vec<String> = left.iter().map(u32::to_string).collect();
        return kept(format!("`pitchfork supervisor stop` returned, but its processes {} still run", pids.join(", ")));
    }
    json!({ "stopped": true, "state_dir": provider.state_dir })
}

/// The running supervisor of `provider` and this Stack state's owner mark, when the process
/// Pitchfork names as that supervisor carries the mark; otherwise why it is not Stack's to stop.
fn owned_supervisor(ctx: &Ctx, bin: &Path, provider: &ProviderRecord) -> std::result::Result<(crate::procinfo::Process, String), String> {
    let mark = supervisor_owner(&ctx.state).map_err(|why| format!("Stack cannot tell whether it started this supervisor: {why}"))?;
    let pid = match mise::supervised(bin, &provider.state_dir, SUPERVISOR_ID)? {
        mise::Supervised::Found { status, pid: Some(pid), .. } if status == "running" => pid,
        other => return Err(format!("cannot identify the running supervisor: Pitchfork reports {other:?} for {SUPERVISOR_ID}")),
    };
    let Some(seen) = crate::procinfo::process(pid) else {
        return Err(format!("supervisor pid {pid} is not running"));
    };
    match crate::procinfo::environ_has(pid, mise::OWNER_VAR, &mark) {
        Some(true) if seen.current().is_some() => Ok((seen, mark)),
        Some(true) => Err(format!("supervisor pid {pid} changed while Stack checked it")),
        Some(false) => Err(format!(
            "this Stack state did not start supervisor pid {pid}: it was running before Stack used it, or something else started it"
        )),
        None => Err(format!("cannot read the environment of supervisor pid {pid} to confirm Stack started it")),
    }
}

/// Why the supervisor of `provider` may still be needed, if it may (see [`stop_owned_supervisor`]).
fn supervisor_in_use(ctx: &Ctx, bin: &Path, provider: &ProviderRecord) -> Option<String> {
    let mine = resolved(&provider.state_dir);
    let sessions = ctx.state.join("sessions");
    let entries = match fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return daemons_in_use(bin, provider),
        Err(e) => return Some(format!("cannot read {}: {e}", sessions.display())),
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => return Some(format!("cannot read {}: {e}", sessions.display())),
        };
        if path.extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let session = match read_json::<Session>(&path) {
            Ok(session) => session,
            Err(e) => return Some(format!("cannot read the session record {}: {}", path.display(), e.message)),
        };
        if session.project == ctx.root {
            continue;
        }
        match &session.provider {
            Some(p) if resolved(&p.state_dir) == mine => {
                return Some(format!("the session of {} uses this supervisor", session.project.display()))
            }
            None if !session.services.is_empty() => {
                return Some(format!("the session of {} does not record which supervisor runs its services", session.project.display()))
            }
            _ => {}
        }
    }
    daemons_in_use(bin, provider)
}

/// A daemon the supervisor tracks that is not over: anything but stopped, failed or errored
/// (an unreported state included), with a live process, or with a schedule. Between runs, and
/// after a failed one, a scheduled daemon is stopped or failed, yet only a running supervisor
/// starts it again, disabled or not (Pitchfork 2.29.0 runs a disabled daemon on its schedule
/// too). One whose schedule cannot be read may have one.
fn daemons_in_use(bin: &Path, provider: &ProviderRecord) -> Option<String> {
    let daemons = match mise::supervisor_daemons(bin, &provider.state_dir) {
        Ok(daemons) => daemons,
        Err(why) => return Some(format!("cannot list the supervisor's daemons: {why}")),
    };
    let daemons: Vec<&mise::Tracked> = daemons.iter().filter(|d| d.id != SUPERVISOR_ID).collect();
    let busy = daemons.iter().find_map(|d| {
        let over = d.status == "stopped" || d.status.starts_with("failed") || d.status.starts_with("errored");
        if !over {
            let status = if d.status.is_empty() { "in a state Pitchfork did not report" } else { &d.status };
            return Some(format!("daemon {} is {status}", d.id));
        }
        d.pid.filter(|p| pid_alive(*p)).map(|pid| format!("daemon {} is {}, but its process {pid} is alive", d.id, d.status))
    });
    busy.or_else(|| {
        daemons.iter().find_map(|d| match mise::schedule(bin, &provider.state_dir, &d.id) {
            Ok(None) => None,
            Ok(Some(cron)) => Some(format!("daemon {} runs on the schedule `{cron}`, which needs the supervisor between runs", d.id)),
            Err(why) => Some(format!("cannot tell whether daemon {} runs on a schedule: {why}", d.id)),
        })
    })
}

/// What is left, up to 5 seconds after `pitchfork supervisor stop` returned, of the supervisor
/// observed as `supervisor`: itself, or a process of its group or a child of it carrying the
/// same owner mark (its `log-sink` helpers).
fn supervisor_left(supervisor: &crate::procinfo::Process, mark: &str) -> Vec<u32> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let mut left: Vec<u32> = supervisor.current().map(|p| p.pid).into_iter().collect();
        left.extend(
            crate::procinfo::carrying(mise::OWNER_VAR, mark)
                .iter()
                .filter(|p| p.group == supervisor.group || p.parent == supervisor.pid)
                .map(|p| p.pid),
        );
        left.sort_unstable();
        left.dedup();
        if left.is_empty() || Instant::now() >= until {
            return left;
        }
        sleep(Duration::from_millis(200));
    }
}

/// This Stack state's owner mark (see [`mise::OWNER_VAR`]): the value Stack 0.1.23 and earlier
/// wrote once to `<state>/supervisor-owner` before starting a Pitchfork supervisor. A record that
/// does not hold such a mark is never trusted: no supervisor is then Stack's.
fn supervisor_owner(state: &Path) -> std::result::Result<String, String> {
    let path = state.join("supervisor-owner");
    match fs::read_to_string(&path) {
        Ok(text) => {
            let mark = text.strip_suffix('\n').unwrap_or(&text);
            let valid = mark.len() == 38
                && mark.starts_with("stack-")
                && mark[6..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if valid { Ok(mark.to_string()) } else { Err(format!("{} does not hold an owner mark Stack wrote", path.display())) }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("{} does not exist", path.display())),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// `path` with its longest existing ancestor resolved, so one supervisor state directory has
/// one name however it was reached (`/tmp` and `/private/tmp` on macOS, a link).
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The lock of the supervisor whose state directory is `state_dir`: a file in that directory,
/// resolved, so every Stack state and checkout that uses the supervisor, by any name, opens the
/// same one.
fn supervisor_lock(state_dir: &Path) -> PathBuf {
    resolved(state_dir).join("stack-supervisor.lock")
}

/// Stop this checkout's services: everything Stack's supervisor runs for it (and the
/// supervisor), or, for a session an older Stack started, its services under Pitchfork.
fn down_locked(ctx: &Ctx) -> Result<DownReport> {
    let session = load(ctx)?;
    down_stopping(ctx, session, None, &commands::Exempt::load(&ctx.state, Some(&ctx.root)))
}

/// What `down` recovering an unreadable record (see [`recover_unreadable`]) works from.
struct RecordRecovery<'a> {
    /// Both session records as read once under the lock.
    copies: &'a [RecordCopy; 2],
    /// The services the supervisor's spec asked to run, which its state must account for.
    asked: Vec<String>,
}

/// [`down_locked`] for `session` (what this checkout recorded, if anything). With `recovery`,
/// the supervisor stops only on the evidence of its own record (see
/// [`supervisor::shutdown_accounted`]), and each session record is removed only while it is
/// still what was read.
///
/// `exempt` says what is a command, never to be signalled (see [`commands::Exempt`]); when this
/// checkout's command registry cannot be read, nothing but the services' recorded processes and
/// groups is stopped.
fn down_stopping(ctx: &Ctx, session: Option<Session>, recovery: Option<&RecordRecovery>, exempt: &commands::Exempt) -> Result<DownReport> {
    let reserved = ports::lookup(&ctx.state, &ctx.root)?;
    // Services always hold a port reservation from compile, and a launch record names any
    // process stack started. With neither, stack owns nothing and the supervisor has nothing to
    // report.
    let owns_services = !reserved.is_empty() || session.as_ref().is_some_and(|s| !s.services.is_empty());
    let legacy = legacy_provider(session.as_ref()).cloned();
    let dir = supervisor_dir(ctx);
    let before = match (&session, &legacy) {
        _ if !owns_services => Vec::new(),
        (Some(session), Some(provider)) => legacy_statuses(session, provider),
        _ => supervisor::daemons(&dir, &[]),
    };
    let mut pids: Vec<(String, u32)> = before
        .iter()
        .filter_map(|d| d.pid.map(|p| (d.name.clone(), p)))
        .collect();
    if let Some(session) = &session {
        pids.extend(
            session
                .services
                .iter()
                .filter_map(|(name, s)| s.pid.map(|p| (name.clone(), p))),
        );
    }
    pids.sort_unstable();
    pids.dedup();

    let mut ports = owned_ports(session.as_ref(), &before, legacy.as_ref());
    ports.sort_unstable();
    ports.dedup();
    ports.retain(|p| *p != 0);

    if let (Some(session), Some(provider)) = (&session, &legacy) {
        let needs_stop = pids.iter().any(|(_, pid)| pid_alive(*pid))
            || before.iter().any(|d| matches!(d.status.as_str(), "running" | "starting"));
        if needs_stop {
            legacy_stop(session, provider)?;
        }
    }
    // Everything Stack's supervisor runs for this checkout, and the supervisor; nothing when it
    // never ran here.
    match recovery {
        Some(recovery) => {
            let token = crate::checkout::token(&ctx.root)
                .ok_or_else(|| StackError::new("state_invalid", "this directory's checkout token cannot be computed; every record was kept"))?;
            supervisor::shutdown_accounted(&dir, &supervisor::Expected { project: &ctx.root, token: &token, asked: &recovery.asked })?
        }
        None => supervisor::shutdown(&dir)?,
    }

    // A port still open once every recorded process is gone is held by something else: when
    // that is this checkout's own service process (see `Stray`), it is stopped here, before
    // anything is confirmed.
    let mut candidates: IndexMap<String, u16> = reserved.clone();
    for port in &ports {
        if !candidates.values().any(|p| p == port) {
            candidates.insert(format!("port {port}"), *port);
        }
    }
    let mut strays: Vec<Stray> = Vec::new();
    let mut others: Vec<crate::procinfo::Process> = Vec::new();
    let mut looked = false;
    let token = crate::checkout::token(&ctx.root);
    // A command carrying the token (a service ran `stack exec`) is never stopped here: `down`
    // reports it as a running command instead (see `crate::commands`).
    let carriers = || {
        if exempt.blind.is_some() {
            return Vec::new();
        }
        let found = token.as_deref().map(|t| crate::procinfo::carrying(crate::checkout::TOKEN_VAR, t)).unwrap_or_default();
        found.into_iter().filter(|p| !exempt.covers(p)).collect::<Vec<_>>()
    };
    let deadline = Instant::now() + crate::process::bounded(STOP_TIMEOUT);
    let leftovers = loop {
        let pids_alive = pids.iter().any(|(_, pid)| pid_alive(*pid));
        if !pids_alive && !looked && exempt.blind.is_none() {
            looked = true;
            strays = self::strays(&ctx.root, &candidates, exempt);
            // With every recorded process gone, anything else still carrying this checkout's
            // token is a service process left behind too, listening or not.
            others = carriers()
                .into_iter()
                .filter(|p| !strays.iter().any(|s| s.chain.iter().chain(&s.members).any(|q| q.same(p))))
                .collect();
            // A group not signalled as one (it holds a command) is looked at again while its
            // members are stopped one by one: a worker a member started since is stopped too,
            // only while it is still in the group, and only while a process identified in that
            // group still holds its number (see `stopping::group_members`).
            let mixed: Vec<(u32, std::cell::RefCell<Vec<crate::procinfo::Process>>)> = strays
                .iter()
                .filter(|s| s.group.is_none())
                .filter_map(|s| {
                    let top = s.chain.last()?;
                    if top.group != top.pid {
                        return None;
                    }
                    // Every process in the group now, commands included, is identified in it
                    // while its leader is seen leading it before and after the look.
                    let leads = || top.current().is_some_and(|now| now.group == top.group);
                    let mut identified: Vec<crate::procinfo::Process> = s.chain.iter().chain(&s.members).filter(|p| p.group == top.group).cloned().collect();
                    if leads() {
                        let members = crate::procinfo::in_group(top.group);
                        if leads() {
                            identified.extend(members.into_iter().filter(|p| !identified.iter().any(|q| q.same(p))).collect::<Vec<_>>());
                        }
                    }
                    Some((top.group, std::cell::RefCell::new(identified)))
                })
                .collect();
            let rescan = || {
                let mut found = crate::stopping::independent(carriers());
                for (p, known) in crate::stopping::group_members(&mixed, &crate::procinfo::in_group, &|p: &crate::procinfo::Process| p.current(), &|p| exempt.covers(p)) {
                    if !found.iter().any(|(q, _)| q.same(&p)) {
                        found.push((p, known));
                    }
                }
                found
            };
            let survivors = stop_strays(&strays, &others, Some(&rescan));
            if !survivors.is_empty() {
                return Err(crate::stopping::unconfirmed(survivors));
            }
        }
        let alive: Vec<Value> = pids
            .iter()
            .filter(|(_, pid)| pid_alive(*pid))
            .map(|(name, pid)| json!({ "service": name, "pid": pid }))
            .chain(
                ports
                    .iter()
                    .filter(|p| accepting(**p))
                    .map(|p| json!({ "port": p })),
            )
            .collect();
        if alive.is_empty() || Instant::now() >= deadline {
            break alive;
        }
        sleep(Duration::from_millis(200));
    };
    if !leftovers.is_empty() {
        return Err(StackError::new("stop_unconfirmed", "services did not stop")
            .hint("retry `stack down`; stopping is safe to repeat")
            .details(leftovers));
    }
    // Nothing is confirmed while a process of this checkout's services lives, including one
    // started since the processes above were found.
    let late = carriers();
    if !late.is_empty() {
        let survivors = stop_strays(&[], &late, Some(&|| crate::stopping::independent(carriers())));
        if !survivors.is_empty() {
            return Err(StackError::new("stop_unconfirmed", "a service process the supervisor no longer tracks did not stop")
                .hint("retry `stack down`; stopping is safe to repeat")
                .details(survivors));
        }
        others.extend(late);
    }

    match recovery {
        Some(recovery) => remove_unchanged(recovery.copies)?,
        None => {
            remove_if_exists(&ctx.session_file())?;
            remove_if_exists(&ctx.index_file())?;
        }
    }
    forget_project_completions(&ctx.state, &ctx.root);
    // Everything stack owned is gone, so whatever still answers on a reserved port is foreign.
    let pinned = pinned_ports(ctx);
    let conflicts = reserved
        .iter()
        .filter(|(_, port)| accepting(**port))
        .map(|(service, port)| conflict(service, *port, pinned.contains(service)))
        .collect();
    Ok(DownReport {
        stopped: pids
            .iter()
            .map(|(name, pid)| json!({ "service": name, "pid": pid }))
            .chain(strays.iter().map(Stray::json))
            .chain(strays.iter().flat_map(|s| &s.members).chain(&others).map(|p| json!({ "pid": p.pid, "unrecorded": true })))
            .collect(),
        confirmed: true,
        conflicts,
        supervisor: None,
        purge: None,
        removed_unreadable: Vec::new(),
        recovery_checked: None,
        forgotten_unrecorded: 0,
        warnings: Vec::new(),
    })
}

/// Ports stack owns, and must therefore see closed: where a running or starting supervisor
/// daemon actually listens, and the recorded port of a process stack started that is still
/// alive, including an older generation's. `mise daemons` reports a daemon's *configured*
/// port, which after a generation change is the new allocation and not where the old process
/// listens, so a daemon whose configured port differs from its recorded one is asked through
/// Pitchfork's own status for its active port. When that cannot be established, the recorded
/// port (else the configured one) is kept: waiting on a port stack may own is a refusal to
/// confirm, never a stop of something foreign. A reserved port nobody stack knows about
/// listens on is a foreign program's: `mise daemons stop` cannot end it and stack must not
/// try, so it is reported as a conflict instead of being waited for.
fn owned_ports(session: Option<&Session>, statuses: &[DaemonStatus], legacy: Option<&ProviderRecord>) -> Vec<u16> {
    let mut ports = Vec::new();
    for d in statuses {
        let supervised = matches!(d.status.as_str(), "running" | "starting") || d.pid.is_some_and(pid_alive);
        if !supervised {
            continue;
        }
        let recorded = session.and_then(|s| s.services.get(&d.name)).map(|r| r.port).filter(|p| *p != 0);
        if recorded.is_some() && recorded == d.port {
            ports.extend(d.port);
            continue;
        }
        let active = match (&d.id, legacy.and_then(|p| Some((p, p.legacy()?)))) {
            (Some(id), Some((p, bin))) => match mise::supervised(bin, &p.state_dir, id) {
                Ok(mise::Supervised::Found { port: Some(active), .. }) => Some(active),
                _ => None,
            },
            _ => None,
        };
        ports.extend(active.or(recorded).or(d.port));
    }
    if let Some(session) = session {
        ports.extend(session.services.values().filter(|s| s.pid.is_some_and(pid_alive)).map(|s| s.port));
    }
    ports
}

/// What Pitchfork reports for each service of a session an older Stack started under it: one
/// `pitchfork status --json <id>` per recorded service, which reads its state and starts no
/// supervisor. The data directory is the recorded one.
fn legacy_statuses(session: &Session, provider: &ProviderRecord) -> Vec<DaemonStatus> {
    let Some(bin) = provider.legacy() else { return Vec::new() };
    session
        .services
        .iter()
        .map(|(name, record)| {
            let (status, pid, port) = match record.provider_id.as_deref().map(|id| mise::supervised(bin, &provider.state_dir, id)) {
                Some(Ok(mise::Supervised::Found { status, pid, port })) => (status, pid, port.or(Some(record.port))),
                Some(Ok(mise::Supervised::NotFound)) => ("stopped".to_string(), None, Some(record.port)),
                Some(Err(_)) => ("unknown".to_string(), record.pid.filter(|p| pid_alive(*p)), Some(record.port)),
                None => (if record.pid.is_some_and(pid_alive) { "unknown" } else { "stopped" }.to_string(), record.pid.filter(|p| pid_alive(*p)), Some(record.port)),
            };
            DaemonStatus { id: record.provider_id.clone(), name: name.clone(), status, pid, port, data_dir: record.data_dir.clone() }
        })
        .collect()
}

/// Ask Pitchfork to stop each service of a session an older Stack started under it (by its
/// qualified id, through the recorded binary and state directory: the project's configuration
/// is not needed), holding that supervisor's lock shared as Stack 0.1.23 did, so a `down` of
/// that version deciding whether to stop the supervisor waits. A failure is reported once the
/// processes are found still running.
fn legacy_stop(session: &Session, provider: &ProviderRecord) -> Result<()> {
    let Some(bin) = provider.legacy() else { return Ok(()) };
    let _lock = SharedLock::shared(&supervisor_lock(&provider.state_dir))?;
    let mut failed = Vec::new();
    for (name, record) in &session.services {
        let Some(id) = record.provider_id.as_deref() else { continue };
        if let Err(e) = mise::stop_supervised(bin, &provider.state_dir, id) {
            if record.pid.is_some_and(pid_alive) {
                failed.push(json!({ "service": name, "output": e }));
            }
        }
    }
    if failed.is_empty() {
        return Ok(());
    }
    Err(StackError::new("stop_failed", "Pitchfork did not stop services an older Stack started under it")
        .hint("retry `stack down`; stopping is safe to repeat")
        .details(failed))
}

fn conflict(service: &str, port: u16, pinned: bool) -> PortConflict {
    PortConflict { service: service.to_string(), port, pinned, holder: ports::holder(port) }
}

/// Services whose port the project pinned, read without resolving anything. Unreadable
/// configuration means no pins: the hint then names `--reassign-ports`, which such a project
/// would reject anyway.
fn pinned_ports(ctx: &Ctx) -> Vec<String> {
    ctx.compile(false)
        .map(|r| r.stack.services.iter().filter(|(_, e)| e.value.fixed_port().is_some()).map(|(n, _)| n.clone()).collect())
        .unwrap_or_default()
}

/// Reserved ports of this checkout that something stack does not own is listening on. A
/// supervisor daemon of the same service, running or starting with that configured port, is
/// stack's own: this runs after `stop_previous`, so a daemon still running belongs to the
/// unchanged generation whose recorded, configured and actual ports agree.
fn port_conflicts(report: &Report, statuses: &[DaemonStatus]) -> Vec<PortConflict> {
    report
        .ports
        .iter()
        .filter(|(name, port)| {
            let owned = statuses.iter().any(|d| {
                d.name == **name
                    && d.port == Some(**port)
                    && (matches!(d.status.as_str(), "running" | "starting") || d.pid.is_some_and(pid_alive))
            });
            !owned && accepting(**port)
        })
        .map(|(name, port)| {
            let pinned = report.stack.services.get(name).is_some_and(|e| e.value.fixed_port().is_some());
            conflict(name, *port, pinned)
        })
        .collect()
}

/// What listens on the port reserved for `service` when it is neither a daemon of that service
/// the supervisor has (as in [`port_conflicts`]) nor a process of this checkout that nothing
/// recorded (see [`stray`]), which `up` stops itself: the program `up` would refuse to start
/// beside.
fn foreign_listener(ctx: &Ctx, report: &Report, statuses: &[DaemonStatus], service: &str) -> Option<PortConflict> {
    let port = *report.ports.get(service)?;
    let owned = statuses
        .iter()
        .any(|d| d.name == service && d.port == Some(port) && (matches!(d.status.as_str(), "running" | "starting") || d.pid.is_some_and(pid_alive)));
    if owned || !accepting(port) {
        return None;
    }
    if crate::checkout::token(&ctx.root).is_some_and(|token| stray(service, port, &token, &commands::Exempt::load(&ctx.state, Some(&ctx.root))).is_some()) {
        return None;
    }
    let pinned = report.stack.services.get(service).is_some_and(|e| e.value.fixed_port().is_some());
    Some(conflict(service, port, pinned))
}

/// `port_conflict`: which service, which port, who holds it, and what frees this checkout.
fn port_conflict_error(conflicts: Vec<PortConflict>) -> StackError {
    let names: Vec<String> = conflicts
        .iter()
        .map(|c| match &c.holder {
            Some(h) => format!("{} (port {}, held by pid {} `{}`)", c.service, c.port, h.pid, h.command),
            None => format!("{} (port {})", c.service, c.port),
        })
        .collect();
    let (pinned, assigned): (Vec<&PortConflict>, Vec<&PortConflict>) = conflicts.iter().partition(|c| c.pinned);
    let mut hints = Vec::new();
    if !assigned.is_empty() {
        hints.push("run `stack up --reassign-ports` to give the conflicting services other ports and carry on (or `stack compile --reassign-ports` to give this checkout all new ports, then `stack up`)".to_string());
    }
    for c in pinned {
        hints.push(format!(
            "service '{}' pins port {} in [override.services]; change or remove the pin, or free the port",
            c.service, c.port
        ));
    }
    StackError::new(
        "port_conflict",
        format!("another program is listening on this checkout's port for {}", names.join(", ")),
    )
    .hint(hints.join("; "))
    .details(conflicts.iter().map(|c| serde_json::to_value(c).expect("conflict serializes")).collect())
}

use crate::stopping::{accepting, Stray};

/// This checkout's service listening on `port` though nothing recorded it, if that can be
/// established. Anything else, including a listener whose environment cannot be read, is not.
/// Each process is observed before and after its environment is read, so the token is never
/// credited to a process that replaced it under the same PID.
///
/// A command (see [`commands::Exempt`]) is never part of a stray, whatever its environment
/// holds: a listener that is one is no stray, and an ancestor that is one ends the chain. A
/// group with any member that is one (whatever that member carries) is not signalled as a
/// group: every other member is signalled one by one, while still in the group, and any that
/// survives makes the stop unconfirmed.
fn stray(service: &str, port: u16, token: &str, exempt: &commands::Exempt) -> Option<Stray> {
    use crate::procinfo::{environ_has, process};
    let owned = |pid: u32| {
        let before = process(pid)?;
        let carries = environ_has(pid, crate::checkout::TOKEN_VAR, token) == Some(true);
        (carries && before.current().is_some() && !exempt.covers(&before)).then_some(before)
    };
    let mut chain = vec![owned(ports::holder(port)?.pid)?];
    while let Some(parent) = Some(chain.last()?.parent).filter(|p| *p > 1).and_then(owned) {
        chain.push(parent);
    }
    let top = chain.last()?;
    let leads = top.group == top.pid;
    // The group is listed only while its leader is seen holding it before and after the look;
    // what the number lists otherwise may be another group's: unproven, never signalled.
    let (in_group, unproven) = match leads.then(|| crate::stopping::bracketed(top, &crate::procinfo::in_group, &|p| p.current())) {
        Some(Ok(members)) => (members, Vec::new()),
        Some(Err(listed)) => (Vec::new(), listed.into_iter().filter(|p| !chain.iter().any(|c| c.same(p)) && !exempt.covers(p)).collect()),
        None => (Vec::new(), Vec::new()),
    };
    let leads = leads && unproven.is_empty() && !in_group.is_empty();
    let mixed = in_group.iter().any(|p| exempt.covers(p));
    let group = (leads && !mixed).then_some(top.group);
    let members = if mixed {
        // Every member but the commands, each signalled while still in the group.
        in_group.into_iter().filter(|p| !chain.iter().any(|c| c.same(p)) && !exempt.covers(p)).collect()
    } else if leads {
        crate::procinfo::carrying(crate::checkout::TOKEN_VAR, token).into_iter().filter(|p| p.group == top.group && !chain.iter().any(|c| c.same(p)) && !exempt.covers(p)).collect()
    } else {
        Vec::new()
    };
    Some(Stray { service: service.to_string(), port: Some(port), chain, group, members, unproven, signal: libc::SIGTERM })
}

/// Strays on `ports` (service, port) of the checkout at `root`.
fn strays(root: &Path, ports: &IndexMap<String, u16>, exempt: &commands::Exempt) -> Vec<Stray> {
    let Some(token) = crate::checkout::token(root) else { return Vec::new() };
    ports.iter().filter(|(_, port)| accepting(**port)).filter_map(|(service, port)| stray(service, *port, &token, exempt)).collect()
}

/// Stop strays and `others` (more processes of this checkout, observed): see
/// [`crate::stopping::stop`], with [`STOP_TIMEOUT`] before the kill.
fn stop_strays(strays: &[Stray], others: &[crate::procinfo::Process], rescan: Option<&dyn Fn() -> Vec<crate::stopping::Found>>) -> Vec<Value> {
    crate::stopping::stop(strays, others, rescan, STOP_TIMEOUT)
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub session: Option<Session>,
    pub checks: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expired: Option<String>,
    /// The session's lease, structured (kind `none` without a session or lease).
    pub lease: LeaseSummary,
    /// The complete compiled configuration changed since the session started.
    pub stale: bool,
    /// Every service verified and the session is current. `stack status` exits 1 when this is
    /// false and the checkout is `started`.
    pub healthy: bool,
    /// Whether anything of this checkout is started or may still run: a session, a service
    /// process the supervisor runs for it, a command Stack started for it still running, a
    /// command registry that cannot be read, or anything in `found`. False only when all of
    /// that was looked for and none is there (a checkout never started, or stopped by a
    /// confirmed `down`): `healthy` is then false (no service was verified) unless the stack
    /// declares no services, and `stack status` exits 0.
    pub started: bool,
    /// Without a session: that the last `down` was not confirmed (its marker), always; and, with
    /// no registered command or service process the supervisor reports either, what the look
    /// `down` relies on found of this checkout anyway (a process carrying its service
    /// token or command mark, a service in its supervisor's record still running), or why that
    /// look could not be made (a supervisor record that cannot be read, no checkout token). Any
    /// entry makes the checkout `started`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<String>,
    /// Without a session: what the supervisor record at this checkout's path still runs for
    /// another checkout (another token: one moved away, or one that was here before this one).
    /// It does not make this checkout `started`; `stack up` here stops it before starting this
    /// checkout's services, and `stack down` here stops it too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_checkout: Vec<String>,
    /// Artifact coverage of the locked releases on this machine's platform. Read from
    /// stack.lock; nothing is fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<artifacts::PlatformSummary>,
    /// Init state of each service, when any declares init tasks (see `init_tasks`).
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub init: IndexMap<String, crate::init_tasks::InitState>,
    /// Commands Stack registered for this checkout (`exec`, `run`, MCP) still running, from
    /// the command registry alone (see `crate::commands`; `down` also finds what they left
    /// running). They do not make it unhealthy; `down` is not confirmed while one runs.
    pub commands: Vec<RunningCommand>,
    /// Why the command registry could not be read, when it could not: `commands` is then
    /// unknown, and `down` is not confirmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands_error: Option<String>,
}

/// Always answers, even for a broken session: diagnosing that state is the point.
pub fn status(ctx: &Ctx) -> Result<StatusReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?;
    let report = ctx.compile(false)?;
    let checks = if report.stack.services.is_empty() {
        Vec::new()
    } else if !has_provider_config(ctx, &report, session.is_some())? {
        not_launched(&report)
    } else {
        let (env, statuses) = (provider_env(ctx, &report)?, observed(ctx, &report, session.as_ref()));
        verify_session(
            ctx,
            &report,
            &env,
            &statuses,
            session.as_ref(),
            &Timings::new("status"),
        )
    };
    let stale = session
        .as_ref()
        .is_some_and(|s| !same_config(s, ctx, &report));
    let artifacts = (!report.pins().is_empty()).then(|| report.current_artifacts());
    // Services of an expired lease still answer, but the next `gc` or `up` stops them: not
    // something to rely on, so not healthy.
    let lease_expired = session
        .as_ref()
        .and_then(|s| s.lease.as_ref())
        .and_then(|l| l.expired(now()))
        .map(|why| format!("{why}; the next `stack gc` or `stack up` stops these services; `stack renew` (or any `stack exec`) renews a TTL lease"));
    // Checking on a session is using it, as `exec` is: a lease that has not run out is
    // renewed. An expired one is left for GC rather than revived, and a session that is stale
    // or did not finish starting is not one to keep alive.
    if let Some(session) = session.as_mut().filter(|s| !s.launching && !stale && lease_expired.is_none()) {
        renew_in_status(ctx, session)?;
    }
    let data_dirs = checks.iter().map(|c| (c.service.clone(), c.data_dir.clone())).collect();
    let init = crate::init_tasks::statuses(&ctx.state, &ctx.root, &report.stack, &data_dirs);
    let (commands, commands_error) = match commands::listed(&ctx.state, &ctx.root) {
        Ok(listed) => listed,
        Err(e) => (Vec::new(), Some(e.message)),
    };
    // The cheap evidence first; only when it says nothing runs is that proven, with the look
    // `down` relies on, before the checkout is called not started.
    // The supervisor's record at this path names the checkout it ran for (its token). One of
    // another checkout (moved away, or here before this one) makes nothing of this one started.
    let record = if session.is_some() { Ok(None) } else { supervisor::read_state(&supervisor_dir(ctx)) };
    let mine = crate::checkout::token(&ctx.root);
    let foreign = match &record {
        Ok(Some(state)) => state.token.is_some() && state.token != mine,
        _ => false,
    };
    let cheap = session.is_some() || (!foreign && checks.iter().any(|c| c.pid.is_some())) || !commands.is_empty() || commands_error.is_some();
    // Without a session, a `down` that did not confirm is always said (what it reported may
    // still run, found by nothing else); the search runs only when nothing else says started.
    let mut found: Vec<String> = if session.is_none() { unconfirmed_down(ctx).into_iter().collect() } else { Vec::new() };
    if !cheap {
        found.extend(still_running(ctx, &record, foreign));
    }
    let other_checkout = match &record {
        Ok(Some(state)) if foreign => other_checkout(state),
        _ => Vec::new(),
    };
    Ok(StatusReport {
        started: cheap || !found.is_empty(),
        found,
        other_checkout,
        commands,
        commands_error,
        init,
        artifacts,
        healthy: !stale && lease_expired.is_none() && checks.iter().all(|c| c.ready),
        stale,
        lease: LeaseSummary::of(session.as_ref().and_then(|s| s.lease.as_ref())),
        lease_expired,
        session,
        checks,
    })
}

/// What of the checkout at `ctx.root` may still run although no session, registered command or
/// supervised service says so, by the evidence `down` confirms a stop with: every service in
/// its supervisor's record (whatever the manifest calls its services now) and that supervisor,
/// and every process carrying the checkout's service token or command mark. A record that
/// cannot be read, or a token that cannot be computed, is reported too: then nothing is proven.
/// Each process environment is read once (`down` searches again while it stops processes, which
/// may start others; a status only reports).
fn still_running(ctx: &Ctx, record: &Result<Option<supervisor::State>>, foreign: bool) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    let dir = supervisor_dir(ctx);
    match record {
        // Another checkout's (see `StatusReport::other_checkout`), not this one's.
        Ok(Some(_)) if foreign => {}
        Ok(Some(state)) => {
            if let Some(p) = state.supervisor.as_ref().filter(|p| p.alive()) {
                seen.push(p.pid);
                found.push(format!("this checkout's supervisor (pid {}) still runs", p.pid));
            }
            for (service, instance) in &state.services {
                if let Some(p) = instance.live() {
                    seen.push(p.pid);
                    found.push(format!("service {service} (pid {}) still runs under this checkout's supervisor", p.pid));
                }
            }
        }
        Ok(None) => {}
        Err(e) => found.push(format!("cannot tell what this checkout's supervisor runs: {}", e.message)),
    }
    if let Err(e) = supervisor::read_spec(&dir) {
        found.push(format!("cannot tell what this checkout's supervisor was asked to run: {}", e.message));
    }
    let token = crate::checkout::token(&ctx.root);
    let mark = crate::checkout::command_mark(&ctx.root);
    let Some(token) = token else {
        found.push("this directory's checkout token cannot be computed, so its processes cannot be looked for".into());
        return found;
    };
    let mut keys = vec![(crate::checkout::TOKEN_VAR, token.as_str())];
    if let Some(mark) = &mark {
        keys.push((crate::checkout::COMMAND_VAR, mark.as_str()));
    }
    for p in crate::procinfo::carrying_any(&keys) {
        if seen.contains(&p.pid) {
            continue;
        }
        // The kernel's command name, never the process's own argv[0], which may carry anything.
        let what = crate::procinfo::named(p.pid).map_or_else(String::new, |n| format!(" `{}`", n.comm));
        found.push(format!("process {}{what} carries this checkout's environment (a service or a command it started); `stack down` stops or reports it", p.pid));
    }
    found
}

/// What the supervisor record at this checkout's path, written for another checkout (another
/// token: a checkout moved away, or one that was here before), still runs.
fn other_checkout(state: &supervisor::State) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(p) = state.supervisor.as_ref().filter(|p| p.alive()) {
        lines.push(format!(
            "the supervisor at this path (pid {}) runs for another checkout that was here (another directory, another token); `stack up` here stops what it runs before starting this checkout's services, and `stack down` here stops it",
            p.pid
        ));
    }
    for (service, instance) in &state.services {
        if let Some(p) = instance.live() {
            lines.push(format!("service {service} (pid {}) runs for that other checkout, not this one", p.pid));
        }
    }
    lines
}

/// Whether the provider can be asked about this checkout. A fresh checkout (a committed
/// stack.lock, no generated config yet) cannot be: an unconfigured `mise daemons` blames its own
/// settings. Without a launch record either, nothing was started from here. With one, the
/// derived config is only missing (deleted, say) and is written again from stack.lock.
fn has_provider_config(ctx: &Ctx, report: &Report, launched: bool) -> Result<bool> {
    if report.output.exists() {
        return Ok(true);
    }
    if !launched {
        return Ok(false);
    }
    ctx.compile(true)?;
    Ok(true)
}

/// The reason of a service no session launched, with the provider asked about it.
const NO_LAUNCH_RECORD: &str = "no launch record; run `stack up`";
/// The same, for a checkout whose provider configuration was never generated.
const NO_LAUNCH_RECORD_HERE: &str = "no launch record in this checkout; run `stack up`";

/// What the check of a service no session launched says beyond that (another program on its
/// port, say), if anything: what `stack status` of a checkout that is not started still shows.
pub fn beyond_not_launched(check: &Check) -> Option<&str> {
    let reason = check.reason.as_deref()?;
    if check.reason_code != Some(ReasonCode::NotLaunched) {
        return Some(reason);
    }
    let rest = [NO_LAUNCH_RECORD, NO_LAUNCH_RECORD_HERE].iter().find_map(|base| reason.strip_prefix(base)).unwrap_or(reason);
    let rest = rest.trim_start_matches("; ");
    (!rest.is_empty()).then_some(rest)
}

fn not_launched(report: &Report) -> Vec<Check> {
    report
        .stack
        .services
        .keys()
        .map(|service| Check {
            service: service.clone(),
            ready: false,
            port: report.ports.get(service).copied(),
            pid: None,
            identity: None,
            reason: Some(NO_LAUNCH_RECORD_HERE.into()),
            reason_code: Some(ReasonCode::NotLaunched),
            withheld: Vec::new(),
            changed_since_start: Vec::new(),
            watch_incomplete: false,
            checked_at: now(),
            data_dir: None,
            provider_id: None,
        })
        .collect()
}

pub struct ExecPlan {
    /// What `--up` (and `--install`) did before planning, reported with the result.
    pub prepared: Vec<Value>,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Complete environment for the command, including poisoned endpoints.
    pub env: IndexMap<String, String>,
    /// Variables that must not be inherited from the caller either.
    pub removed: Vec<String>,
    pub checks: Vec<Check>,
    /// Keeps TTL collection from stopping services until the command finishes.
    pub execution: Option<ExecutionGuard>,
    /// Names of the secrets granted to the command (values are only in `env`).
    pub secrets: Vec<String>,
    /// Requests a grant made that stack declined, such as removing a protected variable.
    pub secret_warnings: Vec<String>,
    /// Replaces granted values in captured output; empty without grants.
    pub redactor: crate::process::Redactor,
    /// A task's copy of the provider configuration it was planned from, which `env` points
    /// `mise run` at. Removed when the plan is dropped, after the command finishes.
    pub task_config: Option<TaskConfig>,
    /// Where the command runs; the project root when `None`.
    pub dir: Option<PathBuf>,
    /// The command's entry in the checkout's command registry (see `crate::commands`), removed
    /// when the plan is dropped or finished, after the command ends. `None` for `stack env`.
    pub command: Option<commands::Registration>,
}

/// Host that can never resolve (RFC 2606), so a poisoned endpoint fails loudly and says why.
pub const UNVERIFIED_HOST: &str = "unverified.stack.invalid";

/// Withheld value for a connection variable of a service with `preset`. `None` only for
/// bindings that name no host (ports, users, databases, passwords), which are removed. Anything
/// that may select a host keeps a value that cannot resolve, whatever host it named: removing it
/// would let the app fall back to a default such as localhost.
fn poison(preset: Option<&str>, var: &str, value: &str) -> Option<String> {
    let upper = var.to_ascii_uppercase();
    if ["PGHOST", "PGHOSTADDR", "PGSERVICE"].contains(&upper.as_str())
        || upper.ends_with("_HOST")
        || upper.ends_with("_HOSTNAME")
    {
        return Some(UNVERIFIED_HOST.into());
    }
    let endpoint = ["_URL", "_URI", "_DSN"].iter().any(|s| upper.ends_with(s));
    if !endpoint
        && (["PGPORT", "PGUSER", "PGDATABASE", "PGPASSWORD"].contains(&upper.as_str())
            || upper.ends_with("_PORT")
            || (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())))
    {
        return None;
    }
    // A value no format below recognises becomes this, which names only the invalid host. A
    // bare host name would not do for Postgres: libpq clients such as psql read it as a
    // database name and connect to the default host.
    let fallback = |port: Option<&str>| {
        let port = port.map(|p| format!(":{p}")).unwrap_or_default();
        match preset {
            Some("postgres") => format!("postgresql://{UNVERIFIED_HOST}{port}"),
            Some("redis") => format!("redis://{UNVERIFIED_HOST}{port}"),
            _ => format!("{UNVERIFIED_HOST}{port}"),
        }
    };
    if let Some((scheme, rest)) = value.split_once("://") {
        if ["postgres", "postgresql"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
        {
            return Some(poison_libpq_uri(scheme, rest));
        }
        let valid_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+-.:".contains(&b));
        return Some(match poison_url(value) {
            Some(url) => url,
            None if valid_scheme => format!("{scheme}://{UNVERIFIED_HOST}"),
            None => fallback(None),
        });
    }
    if let Some(pairs) = libpq_keywords(value).filter(|p| !p.is_empty()) {
        let mut out = vec![format!("host={UNVERIFIED_HOST}")];
        out.extend(
            pairs
                .iter()
                .filter(|(k, _)| !LIBPQ_HOST_KEYS.contains(&k.as_str()))
                .map(|(k, v)| format!("{k}={}", libpq_quote(v))),
        );
        return Some(out.join(" "));
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        let bracketed = host.starts_with('[') && host.ends_with(']');
        let plain = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if (bracketed || plain) && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            return Some(if endpoint {
                fallback(Some(port))
            } else {
                format!("{UNVERIFIED_HOST}:{port}")
            });
        }
    }
    // Unrecognised shape: the whole value is replaced rather than guessing where the host is.
    Some(fallback(None))
}

/// Parameters that let libpq connect somewhere other than the named host: `service` looks one
/// up in a service file, which may set `hostaddr`.
const LIBPQ_HOST_KEYS: [&str; 3] = ["host", "hostaddr", "service"];

/// Replace the host of a URL, keeping credentials, port, path and other query parameters.
/// The fragment is dropped: parsers disagree on whether `#` starts one, and text after it can
/// read as a query (`/db#x?host=...`). URLs without a host may name a socket path instead, so
/// only the scheme is kept.
fn poison_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if url.host_str().unwrap_or_default().is_empty() {
        return None;
    }
    url.set_host(Some(UNVERIFIED_HOST)).ok()?;
    url.set_fragment(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !LIBPQ_HOST_KEYS.iter().any(|h| k.eq_ignore_ascii_case(h)))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
    }
    Some(url.into())
}

/// Rewrite a PostgreSQL URI as libpq reads it, which is not as a WHATWG URL: `#` has no
/// meaning, the user ends at the first `@` before a `/`, several comma-separated hosts may be
/// named, and `?host=`, `?hostaddr=` and `?service=` override them. The result names only the
/// invalid host, with every kept component percent-encoded so that libpq and URL parsers agree
/// on where each one ends. Credentials, the first port, the database and other parameters are
/// kept; anything libpq would reject is dropped.
fn poison_libpq_uri(scheme: &str, rest: &str) -> String {
    let mut out = format!("{scheme}://");
    let mut rest = rest;
    if let Some(at) = rest
        .find(['@', '/'])
        .filter(|i| rest.as_bytes()[*i] == b'@')
    {
        let (user, password) = match rest[..at].split_once(':') {
            Some((user, password)) => (user, Some(password)),
            None => (&rest[..at], None),
        };
        out.push_str(&percent_encode(&percent_decode(user)));
        if let Some(password) = password {
            out.push(':');
            out.push_str(&percent_encode(&percent_decode(password)));
        }
        out.push('@');
        rest = &rest[at + 1..];
    }
    out.push_str(UNVERIFIED_HOST);

    // Hosts and ports, up to the database (`/`) or parameters (`?`).
    let mut port = None;
    let terminator = loop {
        if let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else {
                return out;
            };
            rest = &inner[end + 1..];
        } else {
            rest = &rest[rest.find([':', '/', '?', ',']).unwrap_or(rest.len())..];
        }
        if let Some(after) = rest.strip_prefix(':') {
            let end = after.find(['/', '?', ',']).unwrap_or(after.len());
            if port.is_none() && !after[..end].is_empty() {
                port = Some(&after[..end]);
            }
            rest = &after[end..];
        }
        match rest.chars().next() {
            Some(',') => rest = &rest[1..],
            Some(c @ ('/' | '?')) => {
                rest = &rest[1..];
                break Some(c);
            }
            Some(_) => return out,
            None => break None,
        }
    };
    if let Some(port) = port.filter(|p| p.bytes().all(|b| b.is_ascii_digit())) {
        out.push(':');
        out.push_str(port);
    }
    let params = match terminator {
        Some('/') => {
            let (db, params) = match rest.split_once('?') {
                Some((db, params)) => (db, Some(params)),
                None => (rest, None),
            };
            out.push('/');
            out.push_str(&percent_encode(&percent_decode(db)));
            params
        }
        Some(_) => Some(rest),
        None => None,
    };
    let kept: Vec<String> = params
        .unwrap_or_default()
        .split('&')
        .filter_map(|param| {
            let (key, value) = param.split_once('=')?;
            let key = percent_decode(key);
            (!value.contains('=') && !LIBPQ_HOST_KEYS.contains(&&*String::from_utf8_lossy(&key)))
                .then(|| {
                    format!(
                        "{}={}",
                        percent_encode(&key),
                        percent_encode(&percent_decode(value))
                    )
                })
        })
        .collect();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    out
}

/// Parse a libpq keyword/value connection string (`host = h password='a b'`), or `None` if
/// libpq would reject it.
fn libpq_keywords(value: &str) -> Option<Vec<(String, String)>> {
    let mut chars = value.chars().peekable();
    let mut pairs = Vec::new();
    loop {
        while chars.next_if(libpq_space).is_some() {}
        if chars.peek().is_none() {
            return Some(pairs);
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !libpq_space(c)) {
            key.push(c);
        }
        while chars.next_if(libpq_space).is_some() {}
        chars.next_if_eq(&'=')?;
        while chars.next_if(libpq_space).is_some() {}
        let mut val = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next()? {
                    '\'' => break,
                    '\\' => val.push(chars.next()?),
                    c => val.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !libpq_space(c)) {
                if c == '\\' {
                    if let Some(escaped) = chars.next() {
                        val.push(escaped);
                    }
                } else {
                    val.push(c);
                }
            }
        }
        pairs.push((key, val));
    }
}

/// `isspace` in the C locale, which libpq uses to separate keywords.
fn libpq_space(c: &char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

fn libpq_quote(value: &str) -> String {
    if !value.is_empty() && !value.contains(|c: char| c.is_whitespace() || c == '\'' || c == '\\') {
        return value.into();
    }
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub(crate) fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Encode everything but RFC 3986 unreserved characters, so no delimiter survives.
fn percent_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Every release stack.lock pins, as the provider names them (see [`scratch::pins`]).
fn provider_pins(report: &Report) -> Vec<mise::Pin> {
    report.lock.as_ref().map(scratch::pins).unwrap_or_default()
}

/// The provider's environment for this checkout: Stack's kept answer when it is current, else
/// mise's (see [`mise::provide`]).
/// The command mark is Stack's alone: never taken from the provider (see `crate::commands`).
fn provider_env(ctx: &Ctx, report: &Report) -> Result<IndexMap<String, String>> {
    let mut env = mise::provide_env(&ctx.state, &ctx.cache, &ctx.root, &provider_pins(report))?;
    env.shift_remove(crate::checkout::COMMAND_VAR);
    Ok(env)
}

/// Which services a command needs. Unverified services are always withheld; required ones
/// also make the command fail instead of running.
pub enum Require {
    Nothing,
    All,
    Only(Vec<String>),
}

/// Prepare a command: verify services, withhold unverified endpoints, renew the lease.
pub fn plan_exec(ctx: &Ctx, cmd: &[String], require: &Require) -> Result<ExecPlan> {
    plan_exec_with(ctx, cmd, require, &Grant::default())
}

/// [`plan_exec`], resolving `grant`'s secrets through the stack's pinned fnox once services
/// are verified and endpoints withheld. The names are checked before any provider call.
pub fn plan_exec_with(ctx: &Ctx, cmd: &[String], require: &Require, grant: &Grant) -> Result<ExecPlan> {
    plan(ctx, Target::Command { cmd, require, grant, register: true })
}

/// The environment a command would get now (`stack env`), planned as [`plan_exec`] plans one
/// but with no command to run: nothing is registered as running, and a lease is renewed as
/// `status` renews it.
pub fn plan_env(ctx: &Ctx) -> Result<ExecPlan> {
    plan(ctx, Target::Command { cmd: &["sh".to_string()], require: &Require::Nothing, grant: &Grant::default(), register: false })
}

/// A declared task, planned with every service required. `mise run` keeps the provider's task
/// semantics (templates, shebangs, argument passing); `--skip-deps` stops it from starting the
/// task's daemons itself, since stack verifies them instead.
///
/// The task is granted exactly the secrets it declares, read from the same compile, under the
/// same project lock, as the command and environment: a task edited while this invocation
/// waits for the lock runs with its new definition's grant, never its old one.
///
/// The lock is released before the task runs, and an ordinary compile may then rewrite the
/// generated configuration. `mise run` therefore reads a copy taken under the lock (see
/// [`TaskConfig`]): the plan runs the body, env and tool pins it was planned and granted for,
/// whatever is compiled while it waits or runs. Pins that are not installed are installed from
/// that copy before the task starts (see [`install_task_tools`]). Its `[env]` values are the ones planning read
/// from the project's own file, not evaluated again against the copy, and they still take
/// precedence over the environment a tool sets.
pub fn plan_task(ctx: &Ctx, name: &str, args: &[String], captured: bool) -> Result<ExecPlan> {
    plan(ctx, Target::Task { name, args, captured })
}

/// Plan an exec or task (`plan`) under a deadline of `timeout` from now, which bounds the whole
/// call: waiting for the project lock, compiling, reading the environment, verifying, installing
/// a task's missing pins and resolving grants, then the command. The deadline stays set on this
/// thread while the returned scope lives: keep it while the command runs, which then gets only
/// the time left. Each subprocess planning starts is bounded by it and killed with its process
/// group when it passes, the project lock is released and a task's copy removed. Planning cut
/// short, or finished with no time left for the command, is `timed_out` with nothing run and
/// what was cut short as the `cause`. Without a timeout this is `plan()` with no deadline.
pub fn plan_within(
    timeout: Option<Duration>,
    raise: &str,
    plan: impl FnOnce() -> Result<ExecPlan>,
) -> Result<(ExecPlan, crate::process::DeadlineScope)> {
    // A timeout too long to fix as a point in time cannot pass: no deadline, as without one.
    let at = timeout.and_then(|t| Instant::now().checked_add(t));
    let scope = crate::process::deadline_scope(at);
    let planned = plan();
    let Some(timeout) = timeout.filter(|_| crate::process::expired()) else {
        return planned.map(|plan| (plan, scope));
    };
    let details = match planned {
        Ok(_) => Vec::new(),
        Err(cause) => vec![json!({ "cause": cause })],
    };
    Err(StackError::new("timed_out", format!("the command did not start within {}s; nothing ran", timeout.as_secs()))
        .hint(format!("{raise}: the time counts from the call, including waiting for the project lock, compiling and installing missing pins"))
        .details(details))
}

/// What [`plan`] prepares: an explicit command with its own grant, or a declared task whose
/// command and grant come from the compile that plans it.
enum Target<'a> {
    /// `register`: the command runs once planned, and is recorded as running until it ends.
    Command { cmd: &'a [String], require: &'a Require, grant: &'a Grant, register: bool },
    Task { name: &'a str, args: &'a [String], captured: bool },
}

fn declared_task<'a>(stack: &'a Composed, name: &str) -> Result<&'a Entry<Task>> {
    stack.tasks.get(name).ok_or_else(|| {
        let known: Vec<&str> = stack.tasks.keys().map(String::as_str).collect();
        StackError::new("unknown_task", format!("no task named '{name}'")).hint(if known.is_empty() {
            "this project defines no tasks; add one under [tasks.<name>] in stack.toml".to_string()
        } else {
            format!("tasks: {}", known.join(", "))
        })
    })
}

fn plan(ctx: &Ctx, target: Target<'_>) -> Result<ExecPlan> {
    let mut timings = Timings::new("exec");
    // Planning a command writes only what it writes holding the project's write lock (the
    // compiled configuration and its trust, then the session's record of this execution), so
    // commands in one checkout plan side by side while nothing that changes the project runs.
    // A task also writes its configuration copy and may install its tools: it plans alone.
    let shared = matches!(target, Target::Command { .. });
    let (_alone, _sharing) = if shared {
        (None, Some(crate::state::project_lock_shared(&ctx.state, &ctx.root, true)?))
    } else {
        (Some(project_lock(&ctx.state, &ctx.root)?), None)
    };
    let write_lock = || -> Result<Option<crate::state::FileLock>> {
        if shared { crate::state::project_write_lock(&ctx.state, &ctx.root).map(Some) } else { Ok(None) }
    };
    let mut writing = write_lock()?;
    timings.mark("lock");
    let mut session = load(ctx)?;
    let report = match target {
        Target::Command { .. } => ctx.compile(true)?,
        // An unknown task is refused before the compile writes anything.
        Target::Task { name, .. } => {
            project::compile_checked(&ctx.options(true), &|stack, _| declared_task(stack, name).map(drop))?
        }
    };
    timings.mark("compile");
    let all = Require::All;
    let task_grant;
    let task_cmd: Vec<String>;
    let (cmd, require, grant, origin) = match target {
        Target::Command { cmd, require, grant, .. } => (cmd, require, grant, secrets::Origin::Command),
        Target::Task { name, args, captured } => {
            let task = declared_task(&report.stack, name)?;
            task_cmd = ["mise", "run", "--skip-deps", "--no-timings", name, "--"]
                .into_iter()
                .map(String::from)
                .chain(args.iter().cloned())
                .collect();
            task_grant = Grant { keys: task.value.secrets.clone(), captured };
            // `mise run` evaluates the provider config again and would restore the endpoints
            // that are withheld from unverified services. Only a fully verified stack has none.
            (task_cmd.as_slice(), &all, &task_grant, secrets::Origin::Task(name))
        }
    };
    secrets::validate(&grant.keys, &report.stack, origin)?;
    let required: Vec<&String> = match require {
        Require::Nothing => Vec::new(),
        Require::All => report.stack.services.keys().collect(),
        Require::Only(names) => names.iter().collect(),
    };
    for name in &required {
        if !report.stack.services.contains_key(*name) {
            return Err(unknown_service(&report, name));
        }
    }
    // A pin that is not installed leaves its directory off the provider's PATH, and the
    // command (or anything it starts) would find whatever release is next on PATH. Asked while
    // the provider prepares the environment (or answered with it from Stack's cache), and
    // answered before anything runs on that PATH (identity probes included). A command is
    // refused; a task's missing pins are installed from its configuration copy below, before
    // it starts.
    let pins = provider_pins(&report);
    let trusted = mise::trust_once(&ctx.state, &ctx.root);
    // Written and trusted: from here the provider only reads the configuration, as other
    // planners do.
    drop(writing.take());
    timings.mark("trust");
    let provide = || {
        let (env, missing) = mise::provide(&ctx.state, &ctx.cache, &ctx.root, &pins);
        let observe = !report.stack.services.is_empty();
        (env.map(|env| (env, observe.then(|| observed(ctx, &report, session.as_ref())))), missing)
    };
    let (provided, missing) = match trusted {
        Ok(skipped) => match provide() {
            // mise may have dropped its own record of a configuration Stack remembers trusting
            // (its state was cleared, or `mise trust --untrust` ran): trusted again, and asked
            // once more.
            (Err(_), _) if skipped => {
                let retrust = write_lock().and_then(|_writing| {
                    mise::forget_trust(&ctx.state, &ctx.root);
                    mise::trust_once(&ctx.state, &ctx.root).map(drop)
                });
                match retrust {
                    Ok(()) => provide(),
                    Err(e) => (Err(e), scratch::not_installed(&ctx.cache, &pins)),
                }
            }
            other => other,
        },
        Err(e) => (Err(e), scratch::not_installed(&ctx.cache, &pins)),
    };
    let missing = missing?;
    if let Target::Command { .. } = target {
        require_installed(&missing)?;
    }
    let (mut env, statuses) = provided?;
    timings.mark("env");
    let checks = match statuses {
        None => Vec::new(),
        Some(statuses) => {
            let checks = verify_session(ctx, &report, &env, &statuses, session.as_ref(), &timings);
            timings.mark("verify");
            checks
        }
    };
    // Under the lock, from what this compile wrote, with the provider lock rendered from the
    // stack.lock it validated (never the project's rendered file, which may be missing or
    // stale); removed if planning fails after this. The `[env]` values just read from the
    // project's file stay authoritative: the copy declares each one the plan holds (not the
    // provider selection stack sets itself) as the value the command inherits, so `mise run`
    // neither evaluates it again against the copy nor lets a tool's environment replace it.
    let selection = mise::config_env(&ctx.root);
    let mut task_config = match target {
        Target::Task { .. } => Some(TaskConfig::capture(
            &ctx.root,
            &ctx.cache,
            artifacts::rendered(report.lock.as_ref()).as_deref(),
            |key| env.get(key).filter(|_| !selection.contains_key(key)).cloned(),
            || mise::shell_expands(&ctx.root),
        )?),
        Target::Command { .. } => None,
    };
    if let Some(config) = task_config.as_mut() {
        install_task_tools(ctx, &report, config, &env, &selection, &missing)?;
        // A pin's directory joins the provider's PATH only once it is installed, and a granted
        // secret is resolved through the pinned fnox on that PATH: read the environment again
        // after installing. The project's `[env]` values, which the copy declares, are the same.
        if !missing.is_empty() {
            env = provider_env(ctx, &report)?;
            timings.mark("env");
        }
        // Set before secrets resolve, so no grant can replace them.
        env.extend(config.env());
    }

    let unavailable: Vec<Value> = checks
        .iter()
        .filter(|c| !c.ready && required.contains(&&c.service))
        .map(|c| json!({ "service": c.service, "reason": c.reason }))
        .collect();
    if !unavailable.is_empty() {
        return Err(StackError::new(
            "service_unavailable",
            format!("{} required service(s) not verified", unavailable.len()),
        )
        .hint("run `stack up`, or `stack status` to see why")
        .details(unavailable));
    }

    // Unsetting is not enough: apps often fall back to a default like localhost:5432, which may
    // be some other server. Poison host-bearing values so connections fail loudly instead.
    // A variable only the caller set is still inherited by the command, so it is withheld too.
    let inherited = inherited_env();
    let mut removed = mise::inherited_config_keys();
    let mut seen: Vec<&String> = Vec::new();
    for check in &checks {
        let preset = report
            .stack
            .services
            .get(&check.service)
            .and_then(|s| s.value.preset.as_deref());
        for var in &check.withheld {
            if seen.contains(&var) {
                continue;
            }
            seen.push(var);
            let value = env.get(var).or_else(|| inherited.get(var));
            match poison(preset, var, value.map_or("", String::as_str)) {
                Some(bad) => {
                    env.insert(var.clone(), bad);
                }
                None => {
                    env.shift_remove(var);
                    removed.push(var.clone());
                }
            }
        }
    }
    let unverified: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ready)
        .map(|c| c.service.as_str())
        .collect();
    if !unverified.is_empty() {
        env.insert("STACK_UNVERIFIED".into(), unverified.join(","));
    }
    let granted = if grant.keys.is_empty() {
        None
    } else {
        let resolved = resolve_secrets(ctx, &report, &env, &removed, &checks, grant)?;
        timings.mark("secrets");
        Some(resolved)
    };
    if let Some(resolved) = &granted {
        for var in &resolved.remove {
            env.shift_remove(var);
            removed.push(var.clone());
        }
        for (key, value) in &resolved.set {
            env.insert(key.clone(), value.clone());
        }
    }
    if let Some(config) = task_config.as_mut() {
        config.track_with(|key| match env.get(key) {
            Some(value) => Some(value.clone()),
            None if removed.iter().any(|r| r == key) => None,
            None => std::env::var(key).ok(),
        });
    }
    let (head, args) = cmd
        .split_first()
        .ok_or_else(|| StackError::new("usage", "no command given"))?;
    let program = which_in(env.get("PATH").map(String::as_str), head).ok_or_else(|| {
        StackError::new(
            "command_not_found",
            format!("'{head}' is not on the stack's PATH"),
        )
    })?;

    // Reserve execution before releasing the same lock used by GC. This closes the
    // verify/start race without holding an exclusive lock throughout a long command.
    let mut execution = None;
    let _writing = write_lock()?;
    if shared {
        // Other commands may have planned meanwhile: this one is recorded in the session as it
        // is now. The shared lock kept it the generation verified above; nothing else is.
        let verified = session.as_ref().map(|s| s.id.clone());
        session = load(ctx)?.filter(|fresh| verified.as_ref() == Some(&fresh.id));
    }
    let register = !matches!(target, Target::Command { register: false, .. });
    if let Some(session) = session.as_mut().filter(|s| !s.launching && same_config(s, ctx, &report) && !register) {
        renew_in_status(ctx, session)?;
        env.insert("STACK_SESSION".into(), session.id.clone());
    } else if let Some(session) = session
        .as_mut()
        .filter(|s| !s.launching && same_config(s, ctx, &report))
    {
        if let Some(lease) = session.lease.as_mut() {
            lease.renewed_at = now();
        }
        let token = sha256_hex(format!("{:?}{}", Instant::now(), std::process::id()).as_bytes());
        let consumed = reconcile_executions(&ctx.state, &ctx.root, session);
        session
            .active_executions
            .insert(token.clone(), std::process::id());
        if let Err(e) = save_executions(ctx, session) {
            // A failed project-mirror write must not leave a phantom execution attached
            // to a long-lived MCP coordinator in the authoritative machine index.
            session.active_executions.shift_remove(&token);
            let _ = write_json(&ctx.index_file(), session);
            return Err(e);
        }
        forget_completions(&consumed);
        execution = Some(ExecutionGuard {
            root: ctx.root.clone(),
            state: ctx.state.clone(),
            session_id: session.id.clone(),
            token,
            released: false,
        });
        env.insert("STACK_SESSION".into(), session.id.clone());
    } else {
        env.shift_remove("STACK_SESSION");
        removed.push("STACK_SESSION".into());
    }
    env.insert("STACK_PROJECT".into(), ctx.root.to_string_lossy().into());
    // Every command started for this checkout is registered, whatever its session, under the
    // project lock `down` holds (see `crate::commands`), and carries the checkout's mark; `stack
    // env` is no command, and a shell that loads it is neither.
    // Whatever the provider's environment says: only Stack sets the mark.
    env.shift_remove(crate::checkout::COMMAND_VAR);
    let command = if register {
        if let Some(mark) = crate::checkout::command_mark(&ctx.root) {
            env.insert(crate::checkout::COMMAND_VAR.into(), mark);
        }
        let started_by = match &target {
            Target::Task { name, .. } => commands::StartedBy::Run(name),
            Target::Command { .. } => commands::StartedBy::Exec,
        };
        Some(commands::register(&ctx.state, &ctx.root, started_by, &program)?)
    } else {
        None
    };
    timings.mark("reserve");

    let (secrets, secret_warnings, redactor) = match granted {
        Some(r) => (r.keys, r.warnings, r.redactor),
        None => Default::default(),
    };
    Ok(ExecPlan {
        prepared: Vec::new(),
        program,
        args: args.to_vec(),
        env,
        removed,
        checks,
        execution,
        secrets,
        secret_warnings,
        redactor,
        task_config,
        dir: None,
        command,
    })
}

/// Refuse to start services that never run as root when Stack runs as root, before anything is
/// downloaded: PostgreSQL's `initdb` and server exit at once for root, after every install.
fn refuse_root(report: &Report) -> Result<()> {
    #[cfg(unix)]
    // SAFETY: geteuid only reads the process's credentials.
    let euid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let euid = 1;
    refused_as_root(euid, &report.stack.services)
}

/// [`refuse_root`] for a given effective user id.
fn refused_as_root(euid: u32, services: &IndexMap<String, Entry<Service>>) -> Result<()> {
    if euid != 0 {
        return Ok(());
    }
    let refusing: Vec<&str> = services.iter().filter(|(_, e)| e.value.preset.as_deref() == Some("postgres")).map(|(n, _)| n.as_str()).collect();
    if refusing.is_empty() {
        return Ok(());
    }
    Err(StackError::new("root_unsupported", format!("PostgreSQL ({}) refuses to run as root, and Stack is running as root", refusing.join(", ")))
        .hint("run Stack as an unprivileged user that owns the checkout, e.g. `useradd -m dev && chown -R dev <checkout> && su dev -c 'stack up'`; installs and tools-only commands work as root")
        .with_detail(json!({ "services": refusing, "euid": 0 })))
}

/// Install a task's `missing` pins before it starts, so `mise run` has nothing to install.
/// Run against the task's configuration copy and the lock beside it (rendered from the
/// validated stack.lock), with `stack install`'s checks and split: the provider release, the
/// artifact policy on this platform, then `mise install --locked` for pins checked or exempt
/// here and plain `mise install` for the rest. A refused download is `artifact_mismatch` with
/// stack's remedy, before the task runs, whether its output is captured or not; nothing here
/// reads the task's own output. With every pin installed this asks nothing more of mise.
///
/// The copy declares its `[env]` variables as inherited, so the install is given the values
/// `planned` (the environment planning read, before any grant resolves) holds for exactly
/// those names; the variables that select the copy are set last and cannot be replaced.
///
/// The install's provider state directory is tracked before it runs, so a refused, failed or
/// cut-short install leaves no link to the copy behind it (`selection` is the provider
/// selection every mise command is given).
fn install_task_tools(
    ctx: &Ctx,
    report: &Report,
    config: &mut TaskConfig,
    planned: &IndexMap<String, String>,
    selection: &IndexMap<String, String>,
    missing: &[scratch::Pin],
) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }
    mise::require_in(&ctx.state, &ctx.root, &project::install_requirements(report))?;
    let plan = install_plan(report)?;
    let wanted = |tools: &[String]| -> Vec<String> { tools.iter().filter(|t| missing.iter().any(|p| &p.tool == *t)).cloned().collect() };
    let locked = wanted(&plan.partition.locked);
    let mut plain = wanted(&plan.partition.plain);
    for pin in missing {
        if !locked.contains(&pin.tool) && !plain.contains(&pin.tool) {
            plain.push(pin.tool.clone());
        }
    }
    let mut overrides: Vec<(String, String)> =
        config.declared().iter().filter_map(|key| Some((key.clone(), planned.get(key)?.clone()))).collect();
    overrides.extend(config.env());
    // What the install sees: the overrides, then the selection, then stack's own environment
    // without the provider settings it does not pass on.
    config.track_with(|key| match overrides.iter().rev().find(|(k, _)| k == key) {
        Some((_, value)) => Some(value.clone()),
        None if selection.contains_key(key) => selection.get(key).cloned(),
        None if mise::inherited_config_keys().iter().any(|k| k == key) => None,
        None => std::env::var(key).ok(),
    });
    if !locked.is_empty() {
        mise::install_tools_with(&ctx.root, &overrides, &locked, true)?;
    }
    if !plain.is_empty() {
        mise::install_tools_with(&ctx.root, &overrides, &plain, false)?;
    }
    Ok(())
}

/// `tools_not_installed` naming the pins mise does not report installed, if any. Nothing is
/// installed here.
fn require_installed(missing: &[scratch::Pin]) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = missing.iter().map(|p| format!("{}@{}", p.tool, p.spec.version)).collect();
    Err(StackError::new(
        "tools_not_installed",
        format!("pinned release(s) not installed: {}", names.join(", ")),
    )
    .hint("run `stack install`, or repeat with `stack exec --install` (MCP `install: true`); exec does not fall back to another release on PATH")
    .details(missing.iter().map(|p| json!({ "tool": p.tool, "version": p.spec.version })).collect()))
}

/// The grant's values from the fnox release stack.lock pins, validated against everything this
/// command's environment sets or withholds: compile-time protected names, every service's
/// withheld variables, every variable the provider set, and every variable removed.
fn resolve_secrets(
    ctx: &Ctx,
    report: &Report,
    env: &IndexMap<String, String>,
    removed: &[String],
    checks: &[Check],
    grant: &Grant,
) -> Result<secrets::Resolved> {
    let resolved = report
        .versions
        .iter()
        .find(|v| v.kind == "tool" && v.name == secrets::FNOX)
        .and_then(|v| v.resolved.clone())
        .ok_or_else(|| {
            StackError::new("secret_unavailable", "stack.lock pins no fnox release")
                .hint("run `stack compile`")
                .with_detail(json!({ "step": "locate", "kind": "not_locked", "timed_out": false }))
        })?;
    let spec = report.stack.tools.get(secrets::FNOX).map(|e| e.value.at(resolved.as_str())).unwrap_or_else(|| ToolSpec::new(resolved));
    let request = secrets::Request { root: &ctx.root, cache: &ctx.cache, pin: spec, env, removed };
    let protected = |key: &str| {
        secrets::protected(key, &report.stack).is_some()
            || env.contains_key(key)
            || removed.iter().any(|r| r == key)
            || checks.iter().any(|c| c.withheld.iter().any(|w| w == key))
    };
    secrets::resolve(&request, grant, protected)
}

pub fn renew(ctx: &Ctx) -> Result<Session> {
    renew_with(ctx, None)
}

/// [`renew`], setting the lease's TTL to `ttl_secs` when given (giving a session without a
/// lease one; an owner process, if any, is kept).
pub fn renew_with(ctx: &Ctx, ttl_secs: Option<u64>) -> Result<Session> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?.ok_or_else(|| {
        StackError::new("no_session", "no session for this project").hint("run `stack up`")
    })?;
    if let (None, Some(ttl)) = (&session.lease, ttl_secs) {
        session.lease = Some(Lease { ttl_secs: Some(ttl), owner_pid: None, renewed_at: now() });
    }
    let lease = session.lease.as_mut().ok_or_else(|| {
        StackError::new(
            "no_lease",
            "this session has no lease; it lives until `stack down`",
        )
        .hint("run `stack renew --ttl 30m` to give it one, or `stack up --ttl 30m`")
    })?;
    if ttl_secs.is_some() {
        lease.ttl_secs = ttl_secs;
    }
    lease.renewed_at = now();
    save(ctx, &session)?;
    Ok(session)
}

/// `status`'s renewal, under its lock: completion records are applied first, as `exec` applies
/// them, so the renewal is never older than one they hold.
fn renew_in_status(ctx: &Ctx, session: &mut Session) -> Result<()> {
    if session.lease.is_none() {
        return Ok(());
    }
    let consumed = reconcile_executions(&ctx.state, &ctx.root, session);
    if let Some(lease) = session.lease.as_mut() {
        lease.renewed_at = lease.renewed_at.max(now());
    }
    // Only the renewal and the executions in progress change, as when `exec` registers one.
    save_executions(ctx, session)?;
    forget_completions(&consumed);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct GcEntry {
    pub project: PathBuf,
    pub reason: String,
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Per-service outcomes for a gone project.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<GcService>,
}

#[derive(Debug, Serialize)]
pub struct GcService {
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
}

/// Commands a person runs to finish what GC refused to do itself. They are POSIX shell
/// commands with every recorded value quoted, so they can be copied as they are.
#[derive(Debug, Clone, Serialize)]
pub struct Recovery {
    /// Shows what runs now: under the recorded supervisor id, or as the recorded pid.
    pub inspect: String,
    /// Offered only while the supervisor still reports the recorded pid and port. Run it only
    /// after `inspect` shows the same pid: nothing makes the check and the stop atomic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_port: Option<u16>,
}

/// Why a gone project's service keeps its record, and what a person can do about it.
struct Unconfirmed {
    why: String,
    recovery: Option<Recovery>,
}

impl From<String> for Unconfirmed {
    fn from(why: String) -> Self {
        Self { why, recovery: None }
    }
}

/// Reclaim sessions whose lease expired or whose project directory is gone.
pub fn gc(state: &Path) -> Result<Vec<GcEntry>> {
    gc_pass::gc_with(state, &GcOptions::default()).map(|pass| pass.reclaimed)
}

/// The locked part of GC for one indexed session at `path`: re-read it, settle executions,
/// and reclaim it if it is still expired or its project gone. The caller holds the lock.
fn gc_locked(state: &Path, path: &Path, stop_orphans: bool) -> Result<Option<GcEntry>> {
    // Re-read after locking: another command may have renewed or replaced this generation.
    if !path.exists() {
        return Ok(None);
    }
    let Some(mut session) = session_record(path)? else { return Ok(None) };
    let ctx = Ctx {
        root: session.project.clone(),
        cache: PathBuf::new(),
        state: state.to_path_buf(),
    };
    let consumed = reconcile_executions(state, &ctx.root, &mut session);
    // Saved only for a project still there: a save would recreate a deleted checkout's
    // `.stack/`. A gone project's completion records go with its index.
    if !consumed.is_empty() && project_gone(&session).is_none() {
        save(&ctx, &session)?;
        forget_completions(&consumed);
    }
    // TTL describes idle time; an executing command is not idle. Owner death still
    // follows the explicit runner policy even if one of its commands has survived.
    let owner_dead = session
        .lease
        .as_ref()
        .and_then(|l| l.owner_pid)
        .is_some_and(|p| !pid_alive(p));
    let busy = !owner_dead && has_active_executions(&session);
    if let Some(reason) = project_gone(&session) {
        if busy {
            return Ok(None);
        }
        return Ok(Some(reclaim_gone(state, path, session, reason, stop_orphans)));
    }
    let Some(reason) = session.lease.as_ref().and_then(|l| l.expired(now())) else {
        return Ok(None);
    };
    if busy {
        return Ok(None);
    }
    let result = down_locked(&ctx);
    Ok(Some(GcEntry {
        project: session.project,
        reason,
        stopped: result.is_ok(),
        error: result.err().map(|e| e.to_string()),
        services: Vec::new(),
    }))
}

/// What a deleted checkout leaves once no session record of it is left (after `down`, or once
/// GC confirmed its services stopped): its port reservations, released here, and its services'
/// data, which mise keeps under its own state directory and nothing else removes. Data is
/// reported, and removed only with `remove_data`, and only data `up` recorded for that checkout
/// (see [`crate::datadirs`]) whose marker still names it, while no process carrying the
/// checkout's token runs. A checkout whose session record is kept (`gc_incomplete`), and a
/// path that holds a directory again, keep everything.
fn reclaim_deleted(state: &Path, remove_data: bool) -> Result<Vec<GcEntry>> {
    let recorded = |project: &Path| index_path(state, project).exists();
    let mut found: IndexMap<PathBuf, Vec<GcService>> = IndexMap::new();
    let mut note = |project: &Path, service: &str, outcome: String| {
        found.entry(project.to_path_buf()).or_default().push(GcService { service: service.into(), outcome: Some(outcome), error: None, recovery: None });
    };
    for reservation in crate::ports::release_gone(state, recorded)? {
        // What mise answered for it was kept with its reservations (see `env_cache`).
        crate::provider::env_cache::forget(state, &reservation.project);
        note(&reservation.project, &reservation.service, format!("port {} released", reservation.port));
    }
    let mut settled = Vec::new();
    for entry in crate::datadirs::entries(state)? {
        if entry.project.exists() || recorded(&entry.project) {
            continue;
        }
        // A checkout created at the path meanwhile takes this lock for `up`; checked again under it.
        let _guard = project_lock(state, &entry.project)?;
        if entry.project.exists() {
            continue;
        }
        let (outcome, done) = reclaim_data(&entry, remove_data);
        if done {
            settled.push(entry.clone());
        }
        note(&entry.project, &entry.service, outcome);
    }
    crate::datadirs::forget(state, |e| settled.contains(e))?;
    // A deleted checkout's supervisor directory (its logs), once nothing it recorded runs and
    // no session of it is left.
    // A supervisor still running for it (every service stopped one by one, then the checkout
    // removed) is shut down first, under the checkout's lock, as `down` would.
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    for (dir, project) in supervisor::orphaned_dirs(state) {
        if recorded(&project) {
            continue;
        }
        let _guard = project_lock(state, &project)?;
        if project.exists() || recorded(&project) {
            continue;
        }
        match supervisor::shutdown(&dir) {
            Ok(()) if supervisor::forget(&dir) => note(&project, "supervisor", format!("Stack's supervisor records and logs in {} removed", dir.display())),
            Ok(()) => {}
            Err(e) => failed.push((project, format!("Stack's supervisor in {} was kept: {e}", dir.display()))),
        }
    }
    // A deleted checkout's init markers go with its data; those of data still recorded stay.
    if !settled.is_empty() {
        let left = crate::datadirs::entries(state)?;
        let mut projects: Vec<&Path> = settled.iter().map(|e| e.project.as_path()).collect();
        projects.dedup();
        for project in projects {
            let kept: Vec<String> = left.iter().filter(|e| e.project == project).map(|e| e.service.clone()).collect();
            let _ = crate::init_tasks::forget(state, project, &kept);
        }
    }
    for (project, why) in failed {
        found.entry(project).or_default().push(GcService { service: "supervisor".into(), outcome: None, error: Some(why), recovery: None });
    }
    Ok(found
        .into_iter()
        .map(|(project, services)| {
            let errors: Vec<String> = services.iter().filter_map(|s| s.error.clone()).collect();
            GcEntry { project, reason: "project directory deleted".into(), stopped: errors.is_empty(), error: (!errors.is_empty()).then(|| errors.join("; ")), services }
        })
        .collect())
}

/// `down --purge`, once `down` confirmed every service of this checkout stopped: remove each
/// data directory `up` recorded for this very directory (by its checkout token; data of an
/// earlier directory at the path is not its own), each only as `gc --reclaim-data` removes a
/// deleted checkout's (see [`reclaim_data`]: the recorded directory, reached with no link
/// followed, its marker naming the checkout, the recorded supervisor confirming the service is
/// not running and no process carrying the token), then release its port reservations and
/// identity tokens. Data Stack never recorded is not touched.
fn purge_locked(ctx: &Ctx) -> Result<PurgeReport> {
    let token = crate::checkout::token(&ctx.root);
    let mut data = Vec::new();
    let mut settled = Vec::new();
    for entry in crate::datadirs::entries(&ctx.state)? {
        if entry.project != ctx.root || token.as_deref() != Some(entry.checkout.as_str()) {
            continue;
        }
        let (outcome, done) = reclaim_data(&entry, true);
        data.push(PurgedData { service: entry.service.clone(), dir: entry.dir.clone(), outcome, kept: !done });
        if done {
            settled.push(entry);
        }
    }
    crate::datadirs::forget(&ctx.state, |e| settled.contains(e))?;
    // Data removed was never initialised as far as the next `up` is concerned; kept data was.
    let kept: Vec<String> = data.iter().filter(|d| d.kept).map(|d| d.service.clone()).collect();
    crate::init_tasks::forget(&ctx.state, &ctx.root, &kept)?;
    // The supervisor's own record and the services' logs go with the checkout's data.
    supervisor::forget(&supervisor_dir(ctx));
    crate::provider::env_cache::forget(&ctx.state, &ctx.root);
    let released_ports = ports::release(&ctx.state, &ctx.root)?
        .into_iter()
        .map(|r| json!({ "service": r.service, "port": r.port }))
        .collect();
    Ok(PurgeReport {
        complete: data.iter().all(|d| !d.kept),
        data,
        released_ports,
        released_identities: crate::identity::release(&ctx.state, &ctx.root)?,
    })
}

/// One deleted checkout's recorded data directory: what happened to it, and whether its record
/// is settled (removed now, gone already, or no longer that checkout's).
///
/// The directory is only ever the one `up` recorded, reached by its recorded location with no
/// link followed (see [`crate::datadirs::hold`]); its marker is read, and it is removed, through
/// the handles that check opened, never by looking a path up again.
fn reclaim_data(entry: &crate::datadirs::Entry, remove: bool) -> (String, bool) {
    use crate::datadirs::Found;
    let dir = &entry.dir;
    let shown = dir.display();
    let held = match crate::datadirs::hold(entry) {
        Found::Held(held) => held,
        Found::Gone => return (format!("data directory {shown} is already gone"), true),
        Found::Other(why) => return (format!("data in {shown} left alone: {why}"), true),
        Found::Unbound(why) => {
            return (format!("data kept in {shown}: {why}; check it and remove it by hand if it is no longer needed"), false);
        }
    };
    if held.beside(MARKER_SUFFIX).as_deref().and_then(marker_owner).as_deref() != Some(entry.checkout.as_str()) {
        return (format!("data in {shown} no longer carries this checkout's marker; left alone"), true);
    }
    // Whatever the supervisor says first: a process whose environment cannot be read (another
    // user's, a macOS platform binary's) does not show up as carrying the token.
    let Some(supervisor) = &entry.supervisor else {
        return (format!("data kept in {shown}: no supervisor identity was recorded to confirm its service stopped"), false);
    };
    if let Err(why) = service_stopped(supervisor) {
        return (format!("data kept in {shown}: {why}"), false);
    }
    let running: Vec<u32> = crate::procinfo::carrying(crate::checkout::TOKEN_VAR, &entry.checkout).iter().map(|p| p.pid).collect();
    if !running.is_empty() {
        return (format!("data kept in {shown}: process {running:?} of the deleted checkout still runs; stop it, then run `stack gc` again"), false);
    }
    if !remove {
        return (format!("data kept in {shown} ({} bytes); `stack gc --reclaim-data` removes it", held.size()), false);
    }
    match held.remove(MARKER_SUFFIX) {
        Ok(bytes) => (format!("data in {shown} removed ({bytes} bytes)"), true),
        Err(e) => (format!("data in {shown} could not be removed: {e}"), false),
    }
}

/// [`project_gone`], for the machine-wide commands (see [`crate::fleet`]).
pub fn checkout_gone(session: &Session) -> Option<&'static str> {
    project_gone(session)
}

/// Why a session's project no longer exists: deleted, or replaced by another directory at the
/// same path (which must never inherit the authority to stop this session's services).
fn project_gone(session: &Session) -> Option<&'static str> {
    if !session.project.exists() {
        return Some("project directory deleted");
    }
    project_replaced(session, &session.project).then_some("project directory replaced")
}

/// Reconcile a gone project without signalling a daemon whose identity can be replaced.
/// Pitchfork has no atomic compare-and-stop operation, so live or uncertain services retain
/// their ownership record. Only confirmed terminal state permits releasing the record.
/// `stop_orphans` (`gc --stop-orphans`) stops a service only where a person would be offered
/// `recovery.stop`, and only once its start time also matches (see [`gc_pass::stop_orphan`]).
fn reclaim_gone(state: &Path, index: &Path, session: Session, reason: &str, stop_orphans: bool) -> GcEntry {
    let mut services = Vec::new();
    let mut problems = Vec::new();
    // Stack's own supervisor stops exactly the processes it recorded (pid and start time), so
    // a gone checkout's services are stopped through it, not left running.
    if let Some(provider) = session.provider.as_ref().filter(|p| p.legacy().is_none()) {
        if let Err(e) = supervisor::shutdown(&provider.state_dir) {
            problems.push(e.to_string());
        }
    }
    for (name, record) in &session.services {
        let service = name.clone();
        let reconciled = match reconcile_gone(session.provider.as_ref(), record, session.launching) {
            Err(refused) if stop_orphans && refused.recovery.as_ref().is_some_and(|r| r.stop.is_some()) => {
                gc_pass::stop_orphan(session.provider.as_ref(), record, refused)
            }
            other => other,
        };
        match reconciled {
            Ok(outcome) => services.push(GcService { service, outcome: Some(outcome), error: None, recovery: None }),
            Err(Unconfirmed { why, recovery }) => {
                problems.push(format!("{name}: {why}"));
                services.push(GcService { service, outcome: None, error: Some(why), recovery });
            }
        }
    }
    if problems.is_empty() {
        match remove_if_exists(index) {
            Ok(()) => forget_project_completions(state, &session.project),
            Err(e) => problems.push(e.to_string()),
        }
    }
    GcEntry {
        project: session.project,
        reason: reason.into(),
        stopped: problems.is_empty(),
        error: (!problems.is_empty()).then(|| problems.join("; ")),
        services,
    }
}

fn reconcile_gone(provider: Option<&ProviderRecord>, record: &ServiceRecord, launching: bool) -> std::result::Result<String, Unconfirmed> {
    let alive = record.pid.is_some_and(pid_alive);
    let listening = record.port != 0 && accepting(record.port);
    // Stack's own supervisor records each process by pid and start time: what it says is
    // proof, and what it left running is stopped through it (see [`reclaim_gone`]).
    if let Some(provider) = provider.filter(|p| p.legacy().is_none()) {
        let state = supervisor::state_lossy(&provider.state_dir);
        let live: Vec<u32> = state.services.values().filter_map(|i| i.live()).map(|p| p.pid).collect();
        if !live.is_empty() || (launching && supervisor::alive(&state)) {
            return Err(String::from("Stack's supervisor still runs this checkout's services; record retained").into());
        }
        let unresolved = supervisor::unresolved(&provider.state_dir);
        if !unresolved.is_empty() {
            return Err(format!("processes of this checkout's services may still run and could not be confirmed stopped ({}); record retained", Value::Array(unresolved)).into());
        }
        return match record.pid {
            Some(pid) if alive && !live.contains(&pid) && state.services.values().any(|i| i.process.as_ref().is_some_and(|p| p.pid == pid)) => {
                Ok(format!("not running (pid {pid} is now another process)"))
            }
            _ if listening => Ok(format!("port {} is held by a process Stack's supervisor does not run; left alone", record.port)),
            _ => Ok("not running".into()),
        };
    }
    let (Some(provider), Some(id)) = (provider, record.provider_id.as_deref()) else {
        return Err(match record.pid {
            Some(pid) if alive => Unconfirmed {
                why: format!("pid {pid} is alive, but no supervisor id was recorded to confirm it is this service; not signalled"),
                recovery: Some(process_recovery(pid, record)),
            },
            _ => String::from("no supervisor identity was recorded; terminal state cannot be confirmed; record retained").into(),
        });
    };
    let Some(bin) = provider.legacy() else {
        return Err(String::from("no supervisor identity was recorded; terminal state cannot be confirmed; record retained").into());
    };
    let supervisor = |stop: bool| Recovery {
        inspect: pitchfork_command(bin, provider, "status", id),
        stop: stop.then(|| pitchfork_command(bin, provider, "stop", id)),
        recorded_pid: record.pid,
        recorded_port: (record.port != 0).then_some(record.port),
    };
    match mise::supervised(bin, &provider.state_dir, id)? {
        mise::Supervised::NotFound => match record.pid {
            _ if launching => Err(String::from("startup was interrupted or failed; absence of a daemon now does not prove that startup cannot still register it; record retained").into()),
            Some(pid) if alive => Err(Unconfirmed {
                why: format!("the supervisor no longer tracks {id}, and pid {pid} is alive (possibly reused); not signalled"),
                recovery: Some(process_recovery(pid, record)),
            }),
            _ if listening => Ok(format!("port {} is held by a process the supervisor does not track; left alone", record.port)),
            _ => Ok("not running".into()),
        },
        mise::Supervised::Found { status, pid, port } => {
            if status == "stopped" && !alive && !pid.is_some_and(pid_alive) && !launching {
                return Ok("not running (supervisor confirms stopped)".into());
            }
            // Whatever runs under the id now may not be this generation: offer only `inspect`.
            let unconfirmed = |why: String| Err(Unconfirmed { why, recovery: Some(supervisor(false)) });
            if status != "running" || !pid.is_some_and(pid_alive) {
                return unconfirmed(format!("supervisor state {status:?} is not confirmed terminal cleanup for {id}; record retained"));
            }
            let Some(recorded) = record.pid else {
                return unconfirmed(format!("{id} is running as pid {}, but no verified pid was recorded for it; not signalled", pid.unwrap_or(0)));
            };
            if pid != Some(recorded) {
                return unconfirmed(format!(
                    "{id} now runs pid {}, not the recorded pid {recorded} (restarted, or the path was reused); not signalled",
                    pid.unwrap_or(0)
                ));
            }
            if port != Some(record.port) {
                return unconfirmed(format!("{id} now uses port {}, not the recorded port {}; not signalled", port.unwrap_or(0), record.port));
            }
            Err(Unconfirmed {
                why: format!(
                    "{id} matches pid {recorded}, but Pitchfork cannot atomically validate and stop that generation; not signalled. \
                     Inspect it, stop it explicitly only if it is still pid {recorded}, and retry GC"
                ),
                recovery: Some(supervisor(true)),
            })
        }
    }
}

/// `PITCHFORK_STATE_DIR=<dir> <pitchfork> <action> -- <id>`: the recorded supervisor, reached
/// the way GC reached it, without the deleted project's configuration.
fn pitchfork_command(bin: &Path, provider: &ProviderRecord, action: &str, id: &str) -> String {
    format!(
        "PITCHFORK_STATE_DIR={} {} {action} -- {}",
        shell_quote(&provider.state_dir.to_string_lossy()),
        shell_quote(&bin.to_string_lossy()),
        shell_quote(id)
    )
}

/// Without a supervisor that vouches for the pid, a person can only look at it.
fn process_recovery(pid: u32, record: &ServiceRecord) -> Recovery {
    Recovery {
        inspect: format!("ps -o pid,lstart,command -p {pid}"),
        stop: None,
        recorded_pid: Some(pid),
        recorded_port: (record.port != 0).then_some(record.port),
    }
}

/// One POSIX shell word for `value`. Words that cannot be misread are left bare for legibility.
fn shell_quote(value: &str) -> String {
    let bare = |c: char| c.is_ascii_alphanumeric() || "_-./,:@+".contains(c);
    if !value.is_empty() && value.chars().all(bare) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// What a person runs to finish the cleanup GC refused, one service per paragraph.
fn recovery_steps(entries: &[GcEntry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|e| e.services.iter())
        .filter_map(|s| {
            let recovery = s.recovery.as_ref()?;
            let recorded = match (recovery.recorded_pid, recovery.recorded_port) {
                (Some(pid), Some(port)) => format!(" (recorded pid {pid}, port {port})"),
                (Some(pid), None) => format!(" (recorded pid {pid})"),
                _ => String::new(),
            };
            let mut step = format!("  {}{recorded}:\n    {}", s.service, recovery.inspect);
            if let Some(stop) = &recovery.stop {
                step.push_str(&format!("\n    {stop}"));
            }
            Some(step)
        })
        .collect()
}

/// Like [`gc`], but fails when any session it tried to reclaim is still running or could not
/// be confirmed stopped. Ownership records are kept for a retry. Then what deleted checkouts
/// with no session record left behind (see [`reclaim_deleted`]): their port reservations are
/// released, and their data reported, or removed with `remove_data`.
/// With `stop_orphans`, a deleted checkout's service is stopped when every recorded fact still
/// matches it (see [`gc_pass::stop_orphan`]).
pub fn gc_checked(state: &Path, remove_data: bool, stop_orphans: bool) -> Result<Vec<GcEntry>> {
    let pass = gc_with(state, &GcOptions { stop_orphans, ..GcOptions::default() })?;
    let mut entries = pass.reclaimed;
    entries.extend(pass.unreadable);
    entries.extend(reclaim_deleted(state, remove_data)?);
    // Postgres templates nothing can use any more (see `pgtemplate`): a cache, removed quietly;
    // `gc --dry-run` lists them.
    crate::pgtemplate::collect(state, false);
    let failed = entries.iter().filter(|e| !e.stopped).count();
    if failed == 0 {
        return Ok(entries);
    }
    Err(StackError::new(
        "gc_incomplete",
        format!("{failed} session(s) could not be confirmed stopped"),
    )
    .hint(gc_hint(&entries))
    .details(entries.iter().map(|e| serde_json::to_value(e).expect("gc entry serializes")).collect()))
}

fn gc_hint(entries: &[GcEntry]) -> String {
    if entries.iter().any(|e| e.reason == gc_pass::UNREADABLE) {
        return "a checkout's session record cannot be read: nothing of it was stopped, reclaimed or released; run `stack down` in that checkout, then `stack gc` again".into();
    }
    let steps = recovery_steps(entries);
    if steps.is_empty() {
        return "retry `stack gc`; for a live project, `stack down` in it. Records are kept until cleanup is confirmed".into();
    }
    let orphans = gc_pass::stop_orphans_hint(entries).map(|h| format!("; {h}")).unwrap_or_default();
    format!(
        "services of a deleted or replaced checkout are never stopped by name. For each service below, run \
         the inspect command; run the stop command, where offered, only if inspect shows the recorded pid. \
         Then retry `stack gc`{orphans}. Records are kept until cleanup is confirmed\n{}",
        steps.join("\n")
    )
}

/// A session record, the machine index's or the project copy: `None` when there is none. One
/// that exists but cannot be read or parsed (left empty or partial by a machine crash, say) is
/// unreadable, a state of its own: never taken as no session, an expired or a stale one, and
/// never overridden by the other copy. Every command that would use it fails `state_invalid`;
/// `gc` reports it and leaves that checkout alone; only `stack down` recovers it (see
/// [`recover_unreadable`]).
pub(crate) fn session_record(path: &Path) -> Result<Option<Session>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable_record(path, &e.to_string())),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| unreadable_record(path, &e.to_string()))
}

/// One session record as `down` read it, once, under the checkout's lock.
#[derive(Debug, Clone, PartialEq)]
enum Captured {
    Absent,
    /// Its bytes, and the file's device and inode.
    Read(Vec<u8>, u64, u64),
    /// It exists but could not be read.
    Failed(String),
}

#[derive(Debug, Clone)]
struct RecordCopy {
    path: PathBuf,
    captured: Captured,
}

impl RecordCopy {
    /// Read `path` once: its bytes and, from the same open file, its identity.
    fn capture(path: &Path) -> Self {
        let read = || -> std::io::Result<Captured> {
            let mut file = fs::File::open(path)?;
            let identity = file_identity(&file.metadata()?);
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)?;
            Ok(Captured::Read(bytes, identity.0, identity.1))
        };
        let captured = match read() {
            Ok(captured) => captured,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Captured::Absent,
            Err(e) => Captured::Failed(e.to_string()),
        };
        Self { path: path.to_path_buf(), captured }
    }

    /// Whether what was read is an unreadable record (see [`session_record`]).
    fn unreadable(&self) -> bool {
        match &self.captured {
            Captured::Absent => false,
            Captured::Read(bytes, ..) => serde_json::from_slice::<Session>(bytes).is_err(),
            Captured::Failed(_) => true,
        }
    }
}

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn file_identity(_: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Remove each of `copies` that is still exactly what was read (same file, same bytes); keep any
/// that changed, appeared or could not be read, and fail naming them.
///
/// A record is never removed by its name: it is first renamed, within its directory, to a
/// private name Stack never reads as a record (see [`private_name`]), and only the file that
/// move took is checked (device, inode, bytes) and unlinked. A file that is not what was read
/// is put back without replacing anything now at the path (a hard link, then the private name
/// unlinked); if something took the path meanwhile, both files stay and both are named.
fn remove_unchanged(copies: &[RecordCopy; 2]) -> Result<()> {
    #[cfg(test)]
    tests::before_unchanged_records_are_removed();
    let mut kept: Vec<String> = Vec::new();
    for copy in copies {
        match &copy.captured {
            Captured::Absent => {
                if !matches!(RecordCopy::capture(&copy.path).captured, Captured::Absent) {
                    kept.push(format!("{} (appeared while `down` ran)", copy.path.display()));
                }
                continue;
            }
            Captured::Failed(_) => {
                kept.push(format!("{} (could not be read)", copy.path.display()));
                continue;
            }
            Captured::Read(..) => {}
        }
        let private = private_name(&copy.path);
        match fs::rename(&copy.path, &private) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                kept.push(format!("{} (removed by something else while `down` ran)", copy.path.display()));
                continue;
            }
            Err(e) => return Err(crate::error::io_error(copy.path.display(), e)),
        }
        if RecordCopy::capture(&private).captured == copy.captured {
            remove_if_exists(&private)?;
            continue;
        }
        #[cfg(test)]
        tests::before_a_record_is_put_back(&copy.path);
        match fs::hard_link(&private, &copy.path) {
            Ok(()) => {
                remove_if_exists(&private)?;
                kept.push(format!("{} (changed while `down` ran)", copy.path.display()));
            }
            Err(_) => kept.push(format!(
                "{} (changed while `down` ran; it is now at {}, and something else took its path, so both were kept)",
                copy.path.display(),
                private.display()
            )),
        }
    }
    if kept.is_empty() {
        return Ok(());
    }
    Err(StackError::new("state_invalid", format!("this checkout's services are stopped, but these records were kept: {}", kept.join("; ")))
        .hint("look at the files named, keep the one that is right, then run `stack down` again: it acts on the records as they are now")
        .details(kept.into_iter().map(|p| json!({ "record": p, "kept": true })).collect()))
}

/// A name beside `path` for a record being removed, which nothing reads as a record: not
/// `*.json` (what the session index lists) and not `session.json` (the project copy).
fn private_name(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let unique = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    path.with_file_name(format!(".{name}.removing-{unique}"))
}

/// The checkout an index record (`sessions/<key>.json`) is of, when another record names it:
/// the supervisor's `spec.json` of that key, or the port registry.
pub(crate) fn checkout_of_record(state: &Path, record: &Path) -> Option<PathBuf> {
    let key = record.file_stem()?.to_str()?;
    if let Some(project) = supervisor::read_spec(&state.join("supervisors").join(key)).ok().flatten().and_then(|s| s.project) {
        return Some(project);
    }
    ports::all(state).ok()?.into_iter().map(|r| r.project).find(|p| project_key(p) == key)
}

/// [`session_record`] of the checkout at `checkout` (named in the error, when known).
pub(crate) fn session_record_of(path: &Path, checkout: Option<&Path>) -> Result<Option<Session>> {
    session_record(path).map_err(|e| match checkout {
        Some(checkout) if is_unreadable(&e) => {
            let mut e = e;
            e.message = format!("{} (checkout {})", e.message, checkout.display());
            e.with_detail(json!({ "checkout": checkout }))
        }
        _ => e,
    })
}

fn unreadable_record(path: &Path, why: &str) -> StackError {
    StackError::new("state_invalid", format!("the session record {} cannot be read ({why})", path.display()))
        .hint(
            "this checkout's services may still run: run `stack down` in it, which stops what its supervisor recorded \
             (each process only while its pid, start time and boot match) and removes the record once that is confirmed; \
             until then nothing is started, stopped or reclaimed for it",
        )
        .with_detail(json!({ "record": path, "unreadable": true }))
}

/// Whether `error` is [`session_record`]'s for an unreadable record.
pub(crate) fn is_unreadable(error: &StackError) -> bool {
    error.code == "state_invalid" && error.details.iter().any(|d| d["unreadable"] == true)
}

/// What clearing an unreadable record with no supervisor directory rests on (see
/// [`recover_unreadable`]): reported as it is, not as proof that nothing runs.
pub const TOOLS_ONLY_CHECKED: &str = "no supervisor record, no port reserved for this checkout, no process seen carrying its token \
    (a process whose environment cannot be read is not seen); commands Stack started for it are looked for as in every `down` \
    (the command registry, and their mark)";

/// `down` for a checkout whose session record is unreadable (see [`session_record`]).
///
/// With a supervisor record of exactly this checkout (its `spec.json` naming this path and this
/// directory's token, and a `state.json` that accounts for every service the spec asked to run,
/// see [`supervisor::accounts_for`]), what that record shows is stopped as `down` stops
/// anything, each process only while its pid, start time and boot match (a record from an
/// earlier boot names nothing alive), and only on that evidence throughout the wait.
///
/// Without any supervisor directory there is nothing to stop, and the record is cleared when
/// what can be checked holds: no supervisor record, no port reserved for this checkout, no
/// process seen carrying its token (a process whose environment cannot be read is not seen).
/// That is not proof that nothing of the checkout runs. Commands Stack started (which never
/// carry the token, only services get it) are looked for as in every `down`, through the
/// command registry and their mark (see `crate::commands`), and one found makes it
/// unconfirmed. The report says what was checked ([`TOOLS_ONLY_CHECKED`]).
///
/// Either way each record is then removed only while it is still what was read; anything else
/// (no evidence, evidence that is not this checkout's, a stop that is not confirmed) fails
/// `state_invalid` and keeps every record, reservation and data directory.
fn recover_unreadable(ctx: &Ctx, copies: &[RecordCopy; 2], purge: bool) -> Result<DownReport> {
    let records: Vec<String> = copies.iter().filter(|c| c.unreadable()).map(|c| c.path.display().to_string()).collect();
    let refuse = |why: String| {
        Err(StackError::new("state_invalid", format!("{} cannot be read, and {why}: nothing was stopped and every record was kept", records.join(" and ")))
            .hint("check what may still run for this checkout (`stack ps`, the processes on its ports) and stop it yourself; then delete the unreadable record by hand")
            .details(records.iter().map(|p| json!({ "record": p, "unreadable": true, "checkout": ctx.root })).collect()))
    };
    let removed = || copies.iter().filter(|c| c.captured != Captured::Absent).map(|c| c.path.clone()).collect();
    let dir = supervisor_dir(ctx);
    let token = crate::checkout::token(&ctx.root);
    let spec = match supervisor::read_spec(&dir) {
        Ok(Some(spec)) => spec,
        Ok(None) if !dir.exists() => {
            let reserved = ports::lookup(&ctx.state, &ctx.root)?;
            let exempt = commands::Exempt::load(&ctx.state, Some(&ctx.root));
            let carriers: Vec<_> = token.as_deref().map(|t| crate::procinfo::carrying(crate::checkout::TOKEN_VAR, t)).unwrap_or_default().into_iter().filter(|p| !exempt.covers(p)).collect();
            if !reserved.is_empty() || token.is_none() || !carriers.is_empty() {
                return refuse(format!(
                    "Stack's supervisor never recorded anything for it, but {}",
                    if !reserved.is_empty() { "ports are reserved for its services" } else { "processes of it may run" }
                ));
            }
            remove_unchanged(copies)?;
            forget_project_completions(&ctx.state, &ctx.root);
            commands::confirm(0, commands::after_down(&ctx.state, &ctx.root, &[], &exempt))?;
            let mut report = DownReport {
                stopped: Vec::new(),
                confirmed: true,
                conflicts: Vec::new(),
                supervisor: None,
                purge: None,
                removed_unreadable: removed(),
                recovery_checked: Some(TOOLS_ONLY_CHECKED.into()),
                forgotten_unrecorded: 0,
                warnings: Vec::new(),
            };
            if purge {
                report.purge = Some(purge_locked(ctx)?);
            }
            return Ok(report);
        }
        Ok(None) => return refuse(format!("no supervisor record of this checkout is in {}", dir.display())),
        Err(e) => return refuse(format!("its supervisor's record cannot be read either ({})", e.message)),
    };
    match supervisor::read_state(&dir) {
        Ok(Some(_)) => {}
        Ok(None) => return refuse(format!("its supervisor never recorded what runs ({} has no state.json)", dir.display())),
        Err(e) => return refuse(format!("its supervisor's record cannot be read either ({})", e.message)),
    }
    if spec.project.as_deref() != Some(ctx.root.as_path()) || token.is_none() || spec.token != token {
        return refuse(format!("the supervisor record in {} is not this checkout's", dir.display()));
    }
    let asked = supervisor::asked_to_run(&spec);
    if let Some(state) = supervisor::read_state(&dir).ok().flatten() {
        if let Err(why) = supervisor::accounts_for(&spec, &state, &asked) {
            return refuse(format!("the supervisor record in {} is not evidence of what runs ({why})", dir.display()));
        }
    }
    let recovery = RecordRecovery { copies, asked };
    let exempt = commands::Exempt::load(&ctx.state, Some(&ctx.root));
    let mut report = down_stopping(ctx, None, Some(&recovery), &exempt).map_err(|e| {
        if e.code == "state_invalid" {
            return e;
        }
        // Not confirmed: the records stay, as the recovery rule requires.
        StackError::new("state_invalid", format!("{} cannot be read, and stopping what its supervisor recorded was not confirmed: every record was kept", records.join(" and ")))
            .hint("retry `stack down`; stopping is safe to repeat")
            .with_detail(json!({ "cause": e }))
    })?;
    commands::confirm(report.stopped.len(), commands::after_down(&ctx.state, &ctx.root, &[], &exempt))?;
    if purge {
        report.purge = Some(purge_locked(ctx)?);
    }
    report.supervisor = Some(json!({ "stopped": true, "state_dir": dir }));
    report.removed_unreadable = removed();
    Ok(report)
}

fn load(ctx: &Ctx) -> Result<Option<Session>> {
    // The machine index is authoritative. A crash between the two atomic writes can
    // leave the project copy behind; lifecycle operations always use the indexed generation.
    // Either one unreadable is a state of its own, whatever the other says (see
    // `session_record`).
    let index = session_record_of(&ctx.index_file(), Some(&ctx.root))?;
    let copy = session_record_of(&ctx.session_file(), Some(&ctx.root))?;
    let (indexed, session) = match (index, copy) {
        (Some(session), _) => (true, session),
        (None, Some(session)) => (false, session),
        (None, None) => return Ok(None),
    };
    // The project copy mirrors the record for this path only. One naming another directory
    // was either copied in with the files (committed to Git, a cloned or duplicated checkout)
    // or came along when this very directory was moved. A copy's services belong to the other
    // directory, whose own record is in the machine index. A move's are this directory's, and
    // that copy may be the last record of them. Either way the copy is discarded only when it
    // cannot be the last record of anything running: it came from another directory, the
    // index of its path still holds it, or the supervisor confirms nothing it names runs.
    if !indexed && session.project != ctx.root {
        let same_directory = session.project_dir_id.is_some() && session.project_dir_id == dir_id(&ctx.root);
        let copied = session.project_dir_id.is_some() && !same_directory;
        if copied || owned_elsewhere(ctx, &session) || confirmed_gone(&session) {
            return Ok(None);
        }
        return Err(StackError::new(
            "session_conflict",
            format!(
                "{} records services started at {}, and this directory may have been moved from there",
                ctx.session_file().display(),
                session.project.display()
            ),
        )
        .hint(format!(
            "if this directory was moved from there, run `stack mv {} {}` to carry its records \
             here (it refuses while services may still run: then move the directory back, run \
             `stack down` there, and move it again); if this file was only copied from that \
             checkout, delete it",
            session.project.display(),
            ctx.root.display()
        )));
    }
    // A different directory now at this path must not adopt (or stop) the old one's services.
    if project_replaced(&session, &ctx.root) {
        let err = StackError::new(
            "session_conflict",
            "a session recorded for a previous directory at this path still owns services",
        );
        return Err(if indexed {
            err.hint("run `stack gc` to reclaim them through the supervisor; it reports anything it cannot confirm")
        } else {
            err.hint(format!(
                "{} came with this directory from an earlier one at the same path; stop that \
                 directory's services if any still run, then delete the file",
                ctx.session_file().display()
            ))
        });
    }
    Ok(Some(session))
}

/// The machine index of the copy's own path still holds this session, so GC and that
/// checkout keep its ownership whether this directory is a copy or was moved from there.
fn owned_elsewhere(ctx: &Ctx, session: &Session) -> bool {
    read_json::<Session>(&index_path(&ctx.state, &session.project)).is_ok_and(|indexed| indexed.id == session.id)
}

/// GC would reclaim this record without stopping anything: no launch is in progress and the
/// supervisor confirms each service it names is gone. Otherwise discarding the record could
/// leave a service nobody can stop.
fn confirmed_gone(session: &Session) -> bool {
    !session.launching
        && session.services.values().all(|r| reconcile_gone(session.provider.as_ref(), r, false).is_ok())
}

/// Record `session` for the checkout at `root`, in the machine index and the checkout's own
/// copy, as every lifecycle command does: for `stack mv` (see [`crate::fleet`]).
pub(crate) fn save_at(state: &Path, root: &Path, session: &Session) -> Result<()> {
    save(&Ctx { root: root.to_path_buf(), cache: PathBuf::new(), state: state.to_path_buf() }, session)
}

fn save(ctx: &Ctx, session: &Session) -> Result<()> {
    save_with(ctx, session, true)
}

/// [`save`] without waiting for the disk, for what changes only the executions in progress
/// (registering one, forgetting it) and when the session was last renewed or verified: a
/// machine crash ends those executions and the services too, and the record it may leave is the
/// one before.
fn save_executions(ctx: &Ctx, session: &Session) -> Result<()> {
    save_with(ctx, session, false)
}

fn save_with(ctx: &Ctx, session: &Session, sync: bool) -> Result<()> {
    let write = if sync { write_json::<Session> } else { crate::state::write_json_unsynced::<Session> };
    write(&ctx.index_file(), session)?;
    write(&ctx.session_file(), session)?;
    // The project copy describes this checkout only; keep it out of version control so clones
    // and worktrees never carry it.
    let ignore = ctx.root.join(".stack").join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "*\n").map_err(|e| crate::error::io_error(ignore.display(), e))?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(crate::error::io_error(path.display(), e)),
    }
}

/// Remove a project's completion records with its session. Best effort: one left behind
/// names a generation that is gone, so it never applies and the next reconcile removes it.
fn forget_project_completions(state: &Path, root: &Path) {
    let _ = fs::remove_dir_all(completions_dir(state, root));
}

/// A command that finished while another command held the project lock, so it could not
/// remove its execution itself. Written without the lock; applied under it by the next
/// lifecycle command, to this execution of this session only.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Completion {
    session: String,
    token: String,
    pid: u32,
    completed_at: u64,
}

/// Where a project's completion records are: `<state>/completed/<project key>/<token>.json`.
pub(crate) fn completions_dir(state: &Path, root: &Path) -> PathBuf {
    state.join("completed").join(project_key(root))
}

/// An execution token as [`plan`] makes it: a SHA-256 in lowercase hex.
fn is_token(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Under the project lock: forget executions whose completion record names this session, the
/// token and the PID recorded for it, renewing the lease to when each finished, then those
/// whose coordinator had exited before the records were read. Returns the records applied;
/// the caller removes them only once a record without those executions is saved, so a crash
/// in between applies them again. A record that can never apply (another generation's, or for
/// a token no longer recorded) is removed; one stack did not write (another name, unreadable,
/// naming another token) is left alone and never applied.
///
/// Records are written without the lock, so the order matters. A coordinator writes its
/// record before it exits: one seen exited before the read has left any record it ever will,
/// and the read finds it. One seen running may write its record and exit after the read; it
/// stays recorded, and the session busy, until the next reconcile applies that record. A
/// liveness check after the read would forget it with its renewal unapplied.
fn reconcile_executions(state: &Path, root: &Path, session: &mut Session) -> Vec<PathBuf> {
    let exited: Vec<String> = session
        .active_executions
        .iter()
        .filter(|(_, pid)| !pid_alive(**pid))
        .map(|(token, _)| token.clone())
        .collect();
    let consumed = apply_completions(state, root, session);
    for token in &exited {
        session.active_executions.shift_remove(token);
    }
    consumed
}

fn apply_completions(state: &Path, root: &Path, session: &mut Session) -> Vec<PathBuf> {
    let entries = fs::read_dir(completions_dir(state, root));
    #[cfg(test)]
    tests::after_completions_read();
    let Ok(entries) = entries else { return Vec::new() };
    let mut consumed = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let Some(token) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")).filter(|t| is_token(t)) else {
            continue;
        };
        let Ok(completion) = fs::read(&path).map_err(drop).and_then(|b| serde_json::from_slice::<Completion>(&b).map_err(drop)) else {
            continue;
        };
        if completion.token != token {
            continue;
        }
        let recorded = session.active_executions.get(token).copied();
        if completion.session == session.id && recorded == Some(completion.pid) {
            session.active_executions.shift_remove(token);
            if let Some(lease) = session.lease.as_mut() {
                lease.renewed_at = lease.renewed_at.max(completion.completed_at);
            }
            consumed.push(path);
        } else if completion.session != session.id || recorded.is_none() {
            let _ = fs::remove_file(&path);
        }
    }
    consumed
}

fn forget_completions(consumed: &[PathBuf]) {
    for path in consumed {
        let _ = fs::remove_file(path);
    }
}

/// [`reconcile_executions`] for a decision that may not save otherwise: the record is saved
/// when a completion applied, then the completions are removed.
fn settle_executions(ctx: &Ctx, session: &mut Session) -> Result<()> {
    let consumed = reconcile_executions(&ctx.state, &ctx.root, session);
    if !consumed.is_empty() {
        save(ctx, session)?;
        forget_completions(&consumed);
    }
    Ok(())
}

/// After [`reconcile_executions`], whose liveness observation this keeps: every execution
/// still recorded was running when its records were read. Checking its PID again here could
/// find it exited after writing a record that read missed.
fn has_active_executions(session: &Session) -> bool {
    !session.active_executions.is_empty()
}

/// `session_busy` for `operation`, naming each Stack process that is running a command in the
/// session (its PID and command line) and what to do.
fn busy(session: &Session, operation: &str) -> StackError {
    let running: Vec<Value> = session
        .active_executions
        .values()
        .map(|pid| {
            let command = Command::new("ps")
                .args(["-o", "args=", "-p", &pid.to_string()])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|c| !c.is_empty());
            json!({ "pid": pid, "command": command })
        })
        .collect();
    let pids: Vec<String> = session.active_executions.values().map(u32::to_string).collect();
    StackError::new(
        "session_busy",
        format!("{} command(s) are still executing in this session, so `{operation}` would stop services under them", running.len()),
    )
    .hint(format!(
        "wait for the stack process(es) running them (pid {}) to finish, then retry `stack {operation}`; `stack down` stops the services anyway, and those commands lose them",
        pids.join(", ")
    ))
    .details(running)
}

/// A note when this checkout's running session was started from a configuration other than
/// `report` (such as an edited `[env]`), for `inspect`. Read-only: no lock, no provider calls.
pub fn stale_session(ctx: &Ctx, report: &Report) -> Option<String> {
    let session = load(ctx).ok()??;
    (!same_config(&session, ctx, report)).then(|| {
        "the running services were started from a different configuration; `stack up` restarts them with this one".to_string()
    })
}

/// Whether `session` was started from the configuration `report` composes. Tasks are no part
/// of it: they change what `stack run` runs, never a service. A session recorded by an earlier
/// release, whose digest included the tasks, still counts while nothing else changed.
///
/// The lock counts without what is the provider's alone (the Pitchfork pin Stack 0.1.23 and
/// earlier added, see `project::without_provider_pitchfork`) and without who last wrote it, so
/// `stack compile` dropping that pin leaves a running session current. Records whose digest
/// was taken over the lock file as it was (before this) are accepted too.
fn same_config(session: &Session, ctx: &Ctx, report: &Report) -> bool {
    [(false, true), (true, true), (false, false), (true, false)]
        .into_iter()
        .any(|(with_tasks, normalized)| session.config_digest == digest_of(ctx, report, with_tasks, normalized))
}

/// The fingerprint a session records: everything that decides what its services run, which
/// leaves out the tasks.
fn config_digest(ctx: &Ctx, report: &Report) -> String {
    digest_of(ctx, report, false, true)
}

fn digest_of(ctx: &Ctx, report: &Report, with_tasks: bool, normalized: bool) -> String {
    let mut stack = serde_json::to_value(&report.stack).expect("composed stack serializes");
    if !with_tasks {
        if let Some(stack) = stack.as_object_mut() {
            stack.remove("tasks");
            if let Some(Value::Array(overrides)) = stack.get_mut("overrides") {
                overrides.retain(|o| o["kind"] != "tasks");
            }
        }
    }
    let mut config =
        json!({ "lock": if normalized { normalized_lock_digest(&ctx.root, &report.stack) } else { lock_digest(&ctx.root) }, "stack": stack, "ports": report.ports });
    // Only when a service has a probe, so records written before probes existed stay current.
    if !report.identities.is_empty() {
        config["identities"] = json!(report.identities);
    }
    // Likewise only when present: stacks without implicit provider env keep their sessions.
    let python = report
        .versions
        .iter()
        .find(|v| v.kind == "tool" && v.name == "python")
        .and_then(|v| v.resolved.as_deref());
    let implicit = mise::implicit_env(&report.stack, python);
    if !implicit.is_empty() {
        config["implicit_env"] = json!(implicit);
    }
    sha256_hex(config.to_string().as_bytes())
}

fn verify_session(
    ctx: &Ctx,
    report: &Report,
    env: &IndexMap<String, String>,
    statuses: &[DaemonStatus],
    session: Option<&Session>,
    timings: &Timings,
) -> Vec<Check> {
    let mut checks = verify_all(&ctx.root, report, env, statuses, timings);
    let reason = match session {
        None => Some((ReasonCode::NotLaunched, NO_LAUNCH_RECORD)),
        Some(s) if s.launching => Some((
            ReasonCode::LaunchIncomplete,
            "`stack up` did not finish verifying this launch (its error says why); run `stack up` to keep waiting, or `stack down`",
        )),
        Some(s) if !same_config(s, ctx, report) => {
            Some((ReasonCode::ConfigChanged, "session configuration changed; run `stack up` to restart and verify it"))
        }
        _ => None,
    };
    for check in &mut checks {
        let changed_process = session
            .and_then(|s| s.services.get(&check.service))
            .is_some_and(|r| r.pid != check.pid);
        // A service that failed its own check says why (its process is gone, its port closed);
        // that the launched process is no longer there only adds to it. One that passed with a
        // process other than the launched one is the changed process.
        let process = match (&check.reason, changed_process) {
            (Some(own), true) => Some((
                check.reason_code.unwrap_or(ReasonCode::ProcessChanged),
                format!("{own}; the process `stack up` started is gone; run `stack up` to start it again"),
            )),
            (None, true) => Some((ReasonCode::ProcessChanged, "service process changed since launch; run `stack up`".into())),
            _ => None,
        };
        if let Some((code, reason)) = reason.map(|(code, why)| (code, String::from(why))).or(process) {
            check.ready = false;
            check.identity = None;
            check.reason = Some(reason);
            check.reason_code = Some(code);
            check.withheld = binding_vars(
                &check.service,
                &report.stack.services[&check.service].value,
                check.port,
                env,
                &inherited_env(),
            );
        }
        // A service that is not running leaves its port to whoever takes it. Said here, where
        // the reader is deciding what to do, rather than only when `stack up` is refused.
        if let Some(conflict) = (!check.ready).then(|| foreign_listener(ctx, report, statuses, &check.service)).flatten() {
            let holder = conflict.holder.map_or_else(|| "another program".to_string(), |h| format!("another program (pid {} `{}`)", h.pid, h.command));
            let way_out = if conflict.pinned { "stop that program first" } else { "stop that program first, or run `stack up --reassign-ports`" };
            let note = format!("{holder} is now listening on its port {}: {way_out}", conflict.port);
            check.reason = Some(match check.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
            });
        }
        let watch = &report.stack.services[&check.service].value.watch;
        let started = session.and_then(|s| s.services.get(&check.service)).and_then(|r| r.started_at_ms);
        if let (false, Some(started)) = (watch.is_empty(), started) {
            (check.changed_since_start, check.watch_incomplete) = changed_since(&ctx.root, watch, started);
        }
    }
    checks
}

/// Most changed files a check names; the first are enough to act on.
const CHANGED_LIMIT: usize = 5;
/// Most directory entries examined per check, so a huge tree cannot stall every command
/// (checks run under the lifecycle lock).
const WATCH_SCAN_LIMIT: usize = 20_000;

/// Files under `watch` (relative to `root`) modified after `started_at_ms`, in the order
/// found, and whether the scan stopped at WATCH_SCAN_LIMIT entries before seeing everything.
/// Directories are walked recursively, skipping hidden entries and common build and
/// dependency directories, and never following symbolic links into directories.
pub(crate) fn changed_since(root: &Path, watch: &[String], started_at_ms: u64) -> (Vec<String>, bool) {
    let modified_after = |meta: &fs::Metadata| {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_millis() > u128::from(started_at_ms))
    };
    let mut changed = Vec::new();
    let mut pending: Vec<PathBuf> = watch.iter().rev().map(|w| root.join(w)).collect();
    // Every entry seen counts, including skipped ones, so one wide directory cannot run long.
    let mut budget = WATCH_SCAN_LIMIT;
    while let Some(path) = pending.pop() {
        if changed.len() >= CHANGED_LIMIT {
            return (changed, false);
        }
        let Ok(meta) = fs::metadata(&path) else { continue };
        if !meta.is_dir() {
            if modified_after(&meta) {
                let shown = path.strip_prefix(root).unwrap_or(&path);
                changed.push(shown.display().to_string());
            }
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else { continue };
        let mut children = Vec::new();
        for entry in entries {
            if budget == 0 {
                return (changed, true);
            }
            budget -= 1;
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "target" | "__pycache__") {
                continue;
            }
            // A link to a directory could loop; a link to a file is checked like a file.
            if entry.file_type().is_ok_and(|t| t.is_symlink()) && entry.path().is_dir() {
                continue;
            }
            children.push(entry.path());
        }
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    (changed, false)
}

/// An execution is registered under the lifecycle lock and removed at command completion.
/// A crashed coordinator is ignored by GC through its PID, without a background heartbeat.
/// One whose removal cannot have the lock in time leaves a [`Completion`] instead.
pub struct ExecutionGuard {
    root: PathBuf,
    state: PathBuf,
    session_id: String,
    token: String,
    released: bool,
}

impl ExecutionGuard {
    /// Forget this execution: in its session under the project lock, or, when another command
    /// holds the lock, by a completion record for the next lifecycle command. An error means
    /// the execution may still be recorded, keeping the session busy while this process lives.
    fn release(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.released, true) {
            return Ok(());
        }
        let _timings = Timings::new("release");
        let _grace = crate::process::grace_scope(RECORD_GRACE);
        let ctx = Ctx {
            root: self.root.clone(),
            cache: PathBuf::new(),
            state: self.state.clone(),
        };
        let completed_at = now();
        // The lock is waited for briefly whatever the call's deadline: an expired one still
        // gets the chance, and none (a CLI `exec` without `--timeout`) never waits unbounded.
        // Shared, without passing the gate, and with the write lock: other commands may be
        // planning or finishing too, and one waiting to change the project waits for this.
        let locked = {
            let _bound = crate::process::deadline_scope(Some(Instant::now() + RELEASE_GRACE));
            crate::state::project_lock_shared(&ctx.state, &ctx.root, false)
                .and_then(|project| crate::state::project_write_lock(&ctx.state, &ctx.root).map(|write| (project, write)))
        };
        let _guard = match locked {
            Ok(guard) => guard,
            // Another command holds the lock past RELEASE_GRACE: the next lifecycle command
            // applies this record.
            Err(e) if e.code == "lock_busy" => {
                let completion = Completion { session: self.session_id.clone(), token: self.token.clone(), pid: std::process::id(), completed_at };
                let path = completions_dir(&ctx.state, &ctx.root).join(format!("{}.json", self.token));
                return write_json(&path, &completion);
            }
            Err(e) => return Err(e),
        };
        // No session, or another generation's: nothing of this execution is recorded.
        let Some(mut session) = load(&ctx)? else {
            return Ok(());
        };
        if session.id != self.session_id {
            return Ok(());
        }
        session.active_executions.shift_remove(&self.token);
        // A `renew` that took the lock while this waited for it may have renewed later.
        if let Some(lease) = session.lease.as_mut() {
            lease.renewed_at = lease.renewed_at.max(completed_at);
        }
        save_executions(&ctx, &session)
    }
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        if let Err(e) = self.release() {
            eprintln!("stack: {}", unreleased(&e));
        }
    }
}

/// What a failed release means for the caller.
pub fn unreleased(e: &StackError) -> String {
    format!("this command is still recorded as running ({e}); `up` and `restart` report session_busy while this process lives, and `stack down` clears it")
}

impl ExecPlan {
    /// Forget the execution now, before reporting the command's result: an error says the
    /// session may still record it.
    pub fn finish(&mut self) -> Result<()> {
        let released = self.execution.as_mut().map_or(Ok(()), ExecutionGuard::release);
        let unregistered = self.command.as_mut().map_or(Ok(()), commands::Registration::finish);
        released.and(unregistered)
    }

    /// Have the next process this thread spawns recorded as this plan's command.
    pub fn record_spawn(&self) -> Option<crate::process::SpawnHook> {
        self.command.as_ref().map(commands::Registration::record_next_spawn)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The digest of stack.lock as far as it decides what runs: without the provider-only Pitchfork
/// pin and without `written_by`.
fn normalized_lock_digest(root: &Path, stack: &Composed) -> String {
    match crate::lock::read(root) {
        Ok(Some(lock)) => {
            let mut lock = project::without_provider_pitchfork(lock, stack);
            lock.written_by = None;
            sha256_hex(&serde_json::to_vec(&lock).expect("lock serializes"))
        }
        _ => String::new(),
    }
}

fn lock_digest(root: &Path) -> String {
    fs::read(root.join(crate::lock::LOCK_FILE))
        .map(|b| sha256_hex(&b))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_found_only_by_the_last_rescan_is_still_stopped() {
        let mut worker = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let observed = crate::procinfo::process(worker.id()).unwrap();
        // The worker shows up only once everything else already looks gone.
        let calls = std::cell::Cell::new(0);
        let rescan = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 { vec![(observed.clone(), crate::stopping::Known::Itself)] } else { Vec::new() }
        };
        let started = Instant::now();
        let survivors = stop_strays(&[], &[], Some(&rescan));
        let elapsed = started.elapsed();
        let _ = worker.kill();
        let _ = worker.wait();
        assert!(survivors.is_empty(), "{survivors:?}");
        assert!(elapsed < Duration::from_secs(5), "waited {elapsed:?}: it was never sent SIGTERM");
    }
    use std::cell::RefCell;

    type PutBackHook = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static AFTER_COMPLETIONS_READ: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static BEFORE_REMOVAL: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static BEFORE_PUT_BACK: RefCell<Option<PutBackHook>> = RefCell::new(None);
    }

    /// Runs, once, what a test scheduled for just before `down` recovering an unreadable record
    /// moves the records aside to remove them.
    pub(super) fn before_unchanged_records_are_removed() {
        if let Some(hook) = BEFORE_REMOVAL.with(|h| h.borrow_mut().take()) {
            hook();
        }
    }

    /// Runs, once, what a test scheduled for just before a moved record that is not what was
    /// read is put back at `path`.
    pub(super) fn before_a_record_is_put_back(path: &Path) {
        if let Some(hook) = BEFORE_PUT_BACK.with(|h| h.borrow_mut().take()) {
            hook(path);
        }
    }

    /// A tools-only checkout (no supervisor, no reserved port) whose index record is empty and
    /// whose project copy is readable; `down` run with `during` happening at the removal.
    fn recovering_with(during: impl FnOnce(&Path, &Path) + 'static) -> (tempfile::TempDir, Ctx, Result<DownReport>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app").canonicalize().unwrap_or_else(|_| {
            fs::create_dir_all(dir.path().join("app")).unwrap();
            dir.path().join("app").canonicalize().unwrap()
        });
        let ctx = Ctx { root: root.clone(), cache: dir.path().join("cache"), state: dir.path().join("state") };
        let session = Session { id: "s".into(), project: root, ..Default::default() };
        fs::create_dir_all(ctx.index_file().parent().unwrap()).unwrap();
        fs::write(ctx.index_file(), "").unwrap();
        fs::create_dir_all(ctx.session_file().parent().unwrap()).unwrap();
        fs::write(ctx.session_file(), serde_json::to_vec(&session).unwrap()).unwrap();
        let (index, copy) = (ctx.index_file(), ctx.session_file());
        BEFORE_REMOVAL.with(|h| *h.borrow_mut() = Some(Box::new(move || during(&index, &copy))));
        let result = down_purging(&ctx, true, false);
        BEFORE_REMOVAL.with(|h| h.borrow_mut().take());
        (dir, ctx, result)
    }

    #[test]
    fn recovery_removes_only_records_still_as_read() {
        // Nothing changes: both go, and the checkout is clear.
        let (_dir, ctx, result) = recovering_with(|_, _| {});
        assert_eq!(result.unwrap().removed_unreadable.len(), 2);
        assert!(!ctx.index_file().exists() && !ctx.session_file().exists());
        // The unreadable record is repaired while `down` runs: the repair is kept, the copy that
        // did not change goes.
        let (_dir, ctx, result) = recovering_with(|index, copy| fs::copy(copy, index).map(drop).unwrap());
        let e = result.unwrap_err();
        assert_eq!(e.code, "state_invalid", "{e:?}");
        assert!(e.message.contains(&format!("{} (changed while `down` ran)", ctx.index_file().display())), "{e:?}");
        assert!(ctx.index_file().exists() && !ctx.session_file().exists());
        // The readable counterpart is replaced while `down` runs: the replacement is kept.
        let (_dir, ctx, result) = recovering_with(|_, copy| {
            let other = Session { id: "replacement".into(), ..serde_json::from_slice(&fs::read(copy).unwrap()).unwrap() };
            fs::write(copy, serde_json::to_vec(&other).unwrap()).unwrap();
        });
        assert_eq!(result.unwrap_err().code, "state_invalid");
        assert!(fs::read_to_string(ctx.session_file()).unwrap().contains("replacement"));
        assert!(!ctx.index_file().exists());
        // Replaced by another file with the same bytes: not the file that was read, so kept.
        let (_dir, ctx, result) = recovering_with(|_, copy| {
            let bytes = fs::read(copy).unwrap();
            let fresh = copy.with_extension("new");
            fs::write(&fresh, bytes).unwrap();
            fs::rename(fresh, copy).unwrap();
        });
        assert_eq!(result.unwrap_err().code, "state_invalid");
        assert!(ctx.session_file().exists());
        // Nothing is left under a private name, and no private name is a record.
        for dir in [ctx.index_file().parent().unwrap(), ctx.session_file().parent().unwrap()] {
            assert!(fs::read_dir(dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".removing-")));
        }
    }

    #[test]
    fn a_record_put_back_never_replaces_what_took_its_path() {
        // The copy is replaced before it is moved aside (so the moved file is not what was read),
        // and another file takes its path before it is put back: both stay, both are named.
        BEFORE_PUT_BACK.with(|h| {
            *h.borrow_mut() = Some(Box::new(|path: &Path| fs::write(path, "{\"id\":\"newcomer\"}").unwrap()));
        });
        let (_dir, ctx, result) = recovering_with(|_, copy| {
            let other = Session { id: "replacement".into(), ..serde_json::from_slice(&fs::read(copy).unwrap()).unwrap() };
            fs::write(copy, serde_json::to_vec(&other).unwrap()).unwrap();
        });
        BEFORE_PUT_BACK.with(|h| h.borrow_mut().take());
        let e = result.unwrap_err();
        assert_eq!(e.code, "state_invalid");
        assert!(e.message.contains("both were kept"), "{e:?}");
        assert!(fs::read_to_string(ctx.session_file()).unwrap().contains("newcomer"), "what took the path was replaced");
        let parked: Vec<PathBuf> = fs::read_dir(ctx.session_file().parent().unwrap()).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().contains(".removing-")).collect();
        assert_eq!(parked.len(), 1, "{parked:?}");
        assert!(fs::read_to_string(&parked[0]).unwrap().contains("replacement"));
        assert!(e.message.contains(&parked[0].display().to_string()), "{e:?}");
        // The private name is not one the index or the project copy is read from.
        assert_ne!(parked[0].extension().and_then(|x| x.to_str()), Some("json"));
    }

    #[test]
    fn recovery_without_a_supervisor_clears_a_record_only_when_no_port_is_reserved() {
        let (_dir, ctx, _) = recovering_with(|_, _| {});
        // A port reserved for this path: services may have been started (by an older Stack, say).
        fs::write(ctx.index_file(), "").unwrap();
        crate::ports::assign(&ctx.state, &ctx.root, &[crate::ports::Request { service: "db".into(), fixed: None }], false).unwrap();
        let e = down_purging(&ctx, true, false).unwrap_err();
        assert_eq!(e.code, "state_invalid");
        assert!(e.message.contains("ports are reserved") && e.message.contains("every record was kept"), "{e:?}");
        assert!(ctx.index_file().exists());
    }

    /// Runs, once, what a test scheduled for just after the completion records directory is
    /// read, before anything is decided from it.
    pub(super) fn after_completions_read() {
        if let Some(hook) = AFTER_COMPLETIONS_READ.with(|h| h.borrow_mut().take()) {
            hook();
        }
    }

    #[test]
    fn a_completion_applies_only_to_the_execution_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("app"));
        let dir = completions_dir(&state, &root);
        fs::create_dir_all(&dir).unwrap();
        let token = |c: char| c.to_string().repeat(64);
        let me = std::process::id();
        let mut session = Session { id: "s1".into(), lease: Some(Lease { ttl_secs: Some(60), owner_pid: None, renewed_at: 100 }), ..Session::default() };
        for t in ['1', '2', '4', '6'] {
            session.active_executions.insert(token(t), me);
        }
        let write = |name: &str, session: &str, token: &str, pid: u32| {
            write_json(&dir.join(name), &Completion { session: session.into(), token: token.into(), pid, completed_at: 500 }).unwrap();
        };
        let json = |t: char| format!("{}.json", token(t));
        write(&json('1'), "s1", &token('1'), me); // applies
        write(&json('2'), "s1", &token('2'), me + 1); // another process's PID: never applies
        write(&json('3'), "s1", &token('3'), me); // no longer recorded: removed
        write(&json('5'), "s0", &token('5'), me); // another generation's: removed
        write(&json('4'), "s1", &token('6'), me); // names another token: left alone
        fs::write(dir.join(json('6')), "{ not json").unwrap(); // unreadable: left alone
        write("short.json", "s1", "short", me); // not a token's name: left alone
        write(&json('7').to_uppercase(), "s1", &token('7').to_uppercase(), me); // nor this
        let consumed = reconcile_executions(&state, &root, &mut session);
        assert_eq!(consumed, [dir.join(json('1'))]);
        assert_eq!(session.active_executions.keys().cloned().collect::<Vec<_>>(), [token('2'), token('4'), token('6')]);
        assert_eq!(session.lease.as_ref().unwrap().renewed_at, 500, "renewed to when it finished");
        forget_completions(&consumed);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        let mut expected = vec![json('2'), json('4'), json('6'), json('7').to_uppercase(), "short.json".to_string()];
        expected.sort();
        assert_eq!(left, expected);
        // A record whose PID does not match is removed once its token is no longer recorded,
        // and a lease is never moved back.
        session.active_executions.shift_remove(&token('2'));
        session.lease.as_mut().unwrap().renewed_at = 900;
        assert!(reconcile_executions(&state, &root, &mut session).is_empty());
        assert!(!dir.join(json('2')).exists());
        assert_eq!(session.lease.as_ref().unwrap().renewed_at, 900);
    }

    #[test]
    fn a_completion_applies_after_its_coordinator_exited() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("app"));
        let dir = completions_dir(&state, &root);
        fs::create_dir_all(&dir).unwrap();
        let exited = |_| {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            child.wait().unwrap();
            child.id()
        };
        let (finished, crashed) = ("f".repeat(64), "c".repeat(64));
        let (finished_pid, crashed_pid): (u32, u32) = (exited(()), exited(()));
        let mut session = Session { id: "s1".into(), lease: Some(Lease { ttl_secs: Some(60), owner_pid: None, renewed_at: 100 }), ..Session::default() };
        session.active_executions.insert(finished.clone(), finished_pid);
        session.active_executions.insert(crashed.clone(), crashed_pid);
        let path = dir.join(format!("{finished}.json"));
        write_json(&path, &Completion { session: "s1".into(), token: finished, pid: finished_pid, completed_at: 500 }).unwrap();
        assert_eq!(reconcile_executions(&state, &root, &mut session), [path]);
        assert_eq!(session.lease.as_ref().unwrap().renewed_at, 500, "renewed to when it finished");
        assert!(session.active_executions.is_empty(), "the one that left no record is forgotten too");
    }

    #[test]
    fn a_command_finishing_while_gc_reads_the_records_keeps_the_session_until_its_record_applies() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("app"));
        fs::create_dir_all(&root).unwrap();
        let dir = completions_dir(&state, &root);
        let (go, staged) = (tmp.path().join("go"), tmp.path().join("staged.json"));
        // A coordinator that, once told, publishes its record atomically and exits, as one
        // that could not take the lock does.
        let mut coordinator = std::process::Command::new("sh")
            .args(["-c", r#"i=0; while [ ! -e "$1" ] && [ $i -lt 1000 ]; do sleep 0.01; i=$((i+1)); done; mkdir -p "$2" && mv "$3" "$2/$4""#, "sh"])
            .arg(&go)
            .arg(&dir)
            .arg(&staged)
            .arg(format!("{}.json", "f".repeat(64)))
            .spawn()
            .unwrap();
        let crashed = {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            child.wait().unwrap();
            child.id()
        };
        let renewed_at = now() - 120;
        let mut session = Session {
            id: "s1".into(),
            project: root.clone(),
            lease: Some(Lease { ttl_secs: Some(60), owner_pid: None, renewed_at }),
            ..Session::default()
        };
        session.active_executions.insert("f".repeat(64), coordinator.id());
        session.active_executions.insert("c".repeat(64), crashed);
        let index = index_path(&state, &root);
        write_json(&index, &session).unwrap();
        let completed_at = now();
        write_json(&staged, &Completion { session: "s1".into(), token: "f".repeat(64), pid: coordinator.id(), completed_at }).unwrap();

        // The command finishes and its coordinator exits just after GC read the records,
        // before GC decides which executions are gone or whether the session is idle.
        AFTER_COMPLETIONS_READ.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::write(&go, "").unwrap();
                assert!(coordinator.wait().unwrap().success());
            }))
        });
        assert!(gc(&state).unwrap().is_empty(), "seen running when the records were read, so still busy");
        assert!(AFTER_COMPLETIONS_READ.with(|h| h.borrow().is_none()), "the coordinator finished inside that window");
        let record = dir.join(format!("{}.json", "f".repeat(64)));
        assert!(record.exists(), "its record waits for the next reconcile");
        assert_eq!(read_json::<Session>(&index).unwrap().lease.unwrap().renewed_at, renewed_at);

        // The next one applies it: the lease counts from when the command finished.
        assert!(gc(&state).unwrap().is_empty());
        for saved in [read_json::<Session>(&index).unwrap(), read_json(&root.join(".stack/session.json")).unwrap()] {
            assert_eq!(saved.lease.unwrap().renewed_at, completed_at);
            assert!(saved.active_executions.is_empty(), "the crashed one is forgotten with it");
        }
        assert!(!record.exists());
    }

    #[test]
    fn an_expired_startup_reports_timed_out_with_the_cut_short_step_and_its_progress() {
        let progress = json!({ "steps": [{ "step": "start", "status": "ok" }, { "step": "verify", "status": "failed", "code": "not_ready" }], "retry_safe": true, "changed": true });
        let cut_short = || {
            StackError::new("not_ready", "1 service(s) failed verification")
                .details(vec![json!({ "service": "web", "reason": "port 1 is not accepting connections" }), progress.clone()])
        };
        let _deadline = crate::process::deadline_scope(Some(Instant::now()));
        let e = startup_timed_out(cut_short(), "up", Duration::from_secs(5));
        assert_eq!(e.code, "timed_out");
        assert_eq!(e.message, "`stack up` did not finish within 5s");
        assert!(e.hint.as_deref().unwrap().contains("`stack down` stops them"), "{e}");
        assert_eq!(e.details.len(), 2);
        assert_eq!(e.details[0]["cause"]["code"], "not_ready");
        assert_eq!(e.details[0]["cause"]["details"], json!([{ "service": "web", "reason": "port 1 is not accepting connections" }]));
        assert_eq!(e.details[1], progress);

        let busy = startup_timed_out(StackError::new("lock_busy", "held"), "restart", Duration::from_secs(1));
        assert_eq!(busy.code, "timed_out");
        assert!(busy.hint.as_deref().unwrap().starts_with("nothing was started"), "{busy}");
        assert_eq!(busy.details.len(), 1);
        let cause = &busy.details[0]["cause"];
        assert_eq!((cause["code"].as_str(), cause["message"].as_str()), (Some("lock_busy"), Some("held")));
        // The cause is a full error: its registry hint and whether retrying can help.
        assert_eq!(cause["retryable"], true);
        assert!(cause["hint"].is_string(), "{cause}");
    }

    #[test]
    fn a_startup_timeout_too_long_to_track_is_refused_not_lifted() {
        let ten_minutes = startup_deadline(DEFAULT_STARTUP_TIMEOUT).unwrap();
        assert!(ten_minutes > Instant::now() + Duration::from_secs(590));
        for timeout in [Duration::from_secs(u64::MAX), Duration::MAX] {
            let e = startup_deadline(timeout).unwrap_err();
            assert_eq!(e.code, "usage", "{e}");
            assert_eq!(e.message, format!("a startup timeout of {}s is too long", timeout.as_secs()));
        }
    }

    #[test]
    fn a_failure_before_the_deadline_is_reported_as_itself() {
        let _deadline = crate::process::deadline_scope(Some(Instant::now() + Duration::from_secs(60)));
        let e = startup_timed_out(StackError::new("start_failed", "no"), "up", Duration::from_secs(60));
        assert_eq!((e.code, e.message.as_str()), ("start_failed", "no"));
    }

    const PG: Option<&str> = Some("postgres");

    /// A poisoned URL must name only the invalid host to WHATWG parsers too.
    fn assert_url_host(value: &str) {
        let url = url::Url::parse(value).unwrap();
        assert_eq!(url.host_str(), Some(UNVERIFIED_HOST), "{value}");
        assert!(url.fragment().is_none(), "{value}");
        assert!(
            !url.query_pairs()
                .any(|(k, _)| LIBPQ_HOST_KEYS.contains(&k.as_ref())),
            "{value}"
        );
    }

    #[test]
    fn poison_replaces_every_url_host_and_keeps_the_rest() {
        for (preset, input, expected) in [
            (
                PG,
                "postgresql://postgres@127.0.0.1:41234/postgres",
                "postgresql://postgres@unverified.stack.invalid:41234/postgres",
            ),
            (
                PG,
                "postgresql://user@[::1]:5432/db",
                "postgresql://user@unverified.stack.invalid:5432/db",
            ),
            (
                PG,
                "postgresql://user@db.internal:5432/db",
                "postgresql://user@unverified.stack.invalid:5432/db",
            ),
            (
                // Credentials and paths that merely contain a host name are left alone.
                PG,
                "postgres://localhost:p%40ss-127.0.0.1@localhost:5432/localhost?sslmode=disable",
                "postgres://localhost:p%40ss-127.0.0.1@unverified.stack.invalid:5432/localhost?sslmode=disable",
            ),
            (
                Some("redis"),
                "redis://localhost:6379",
                "redis://unverified.stack.invalid:6379",
            ),
            (
                Some("redis"),
                "redis://:secret@[::1]:6379/0",
                "redis://:secret@unverified.stack.invalid:6379/0",
            ),
            (
                Some("redis"),
                "rediss://cache.example.internal:6380",
                "rediss://unverified.stack.invalid:6380",
            ),
            (
                // libpq lets query parameters override the authority; they are dropped.
                PG,
                "postgresql://u@127.0.0.1/db?host=10.0.0.5&sslmode=disable",
                "postgresql://u@unverified.stack.invalid/db?sslmode=disable",
            ),
            (
                PG,
                "postgresql:///db?host=/var/run/postgresql",
                "postgresql://unverified.stack.invalid/db",
            ),
            (
                PG,
                "postgresql://u@h/db?ho%73t=127.0.0.1&service=prod&application_name=a%20b",
                "postgresql://u@unverified.stack.invalid/db?application_name=a%20b",
            ),
            (
                // libpq has no fragments: the database is `db#x` and `host` is a parameter.
                PG,
                "postgresql://u@localhost/db#x?host=127.0.0.1",
                "postgresql://u@unverified.stack.invalid/db%23x",
            ),
            (
                PG,
                "postgresql://u@localhost/db#x?hostaddr=127.0.0.1",
                "postgresql://u@unverified.stack.invalid/db%23x",
            ),
            (
                // libpq's user ends at the first `@`; a URL parser's at the last before `/?#`.
                PG,
                "postgresql://localhost#@127.0.0.1/db",
                "postgresql://localhost%23@unverified.stack.invalid/db",
            ),
            (
                PG,
                "postgresql://a@b@localhost:5432/db",
                "postgresql://a@unverified.stack.invalid:5432/db",
            ),
            (
                PG,
                "postgresql://h1:5432,[::1]:5433/db",
                "postgresql://unverified.stack.invalid:5432/db",
            ),
            (
                PG,
                "postgresql://u@[::1:5432/db",
                "postgresql://u@unverified.stack.invalid",
            ),
            (
                // Other URL schemes have fragments; they are dropped so no parser reads a query.
                PG,
                "postgresql+asyncpg://u@localhost/db#x?host=127.0.0.1",
                "postgresql+asyncpg://u@unverified.stack.invalid/db",
            ),
            (
                // No host: the path may be a socket.
                Some("redis"),
                "unix:///tmp/redis.sock",
                "unix://unverified.stack.invalid",
            ),
            (
                PG,
                "jdbc:postgresql://localhost:5432/db",
                "jdbc:postgresql://unverified.stack.invalid",
            ),
        ] {
            assert_eq!(
                poison(preset, "DATABASE_URL", input).as_deref(),
                Some(expected),
                "{input}"
            );
            if !expected.starts_with("jdbc:") {
                assert_url_host(expected);
            }
        }
    }

    #[test]
    fn poison_fails_closed_for_hosts_and_malformed_endpoints() {
        for host in ["127.0.0.1", "localhost", "::1", "db.internal", "/tmp", ""] {
            for var in ["PGHOST", "PGHOSTADDR", "PGSERVICE", "CACHE_HOST"] {
                assert_eq!(poison(PG, var, host).as_deref(), Some(UNVERIFIED_HOST));
            }
        }
        for (preset, var, input, expected) in [
            // Malformed or empty endpoints keep a value that names the invalid host: removal,
            // or a bare word that libpq reads as a database name, would reach the default host.
            (
                PG,
                "DATABASE_URL",
                "",
                "postgresql://unverified.stack.invalid",
            ),
            (
                PG,
                "DATABASE_URL",
                "41234",
                "postgresql://unverified.stack.invalid",
            ),
            (
                PG,
                "DATABASE_URL",
                "localhost:5432",
                "postgresql://unverified.stack.invalid:5432",
            ),
            (
                PG,
                "DATABASE_URL",
                "dbname='unterminated",
                "postgresql://unverified.stack.invalid",
            ),
            (
                Some("redis"),
                "REDIS_URL",
                "",
                "redis://unverified.stack.invalid",
            ),
            (
                PG,
                "DATABASE_URL",
                "host=::1 port=5432 dbname=app",
                "host=unverified.stack.invalid port=5432 dbname=app",
            ),
            (
                PG,
                "DATABASE_URL",
                "port=5432 dbname=app",
                "host=unverified.stack.invalid port=5432 dbname=app",
            ),
            (
                // libpq allows spaces around `=` and quoted values.
                PG,
                "DATABASE_URL",
                "host = localhost dbname = app",
                "host=unverified.stack.invalid dbname=app",
            ),
            (
                PG,
                "DATABASE_URL",
                "host=localhost password='has space\\' q' dbname=app",
                "host=unverified.stack.invalid password='has space\\' q' dbname=app",
            ),
            (
                PG,
                "DATABASE_URL",
                "hostaddr=127.0.0.1\tservice=prod dbname=''",
                "host=unverified.stack.invalid dbname=''",
            ),
            (None, "APP_DB", "tcp(127.0.0.1:41234)/db", UNVERIFIED_HOST),
        ] {
            assert_eq!(
                poison(preset, var, input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
        for authority in ["127.0.0.1:41234", "[::1]:41234", "db.internal:41234"] {
            assert_eq!(
                poison(PG, "APP_DB", authority).as_deref(),
                Some("unverified.stack.invalid:41234")
            );
        }
    }

    #[test]
    fn libpq_keywords_follow_libpq_quoting() {
        assert_eq!(
            libpq_keywords(" a=1  b = 'x y' c=\\'d e='' f=g\\").unwrap(),
            [("a", "1"), ("b", "x y"), ("c", "'d"), ("e", ""), ("f", "g")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        for rejected in ["a", "a=1 b", "a='open", "a='open\\'"] {
            assert!(libpq_keywords(rejected).is_none(), "{rejected}");
        }
        for value in ["", "x", "a b", "it's", "back\\slash", "tab\tbed"] {
            let quoted = format!("k={}", libpq_quote(value));
            assert_eq!(libpq_keywords(&quoted).unwrap()[0].1, value, "{quoted}");
        }
    }

    #[test]
    fn poison_removes_bindings_that_name_no_host() {
        for (var, value) in [
            ("PGPORT", "41234"),
            ("PGUSER", "postgres"),
            ("PGDATABASE", "app"),
            ("PGPASSWORD", "secret"),
            ("POSTGRES_PORT", "41234"),
            ("ANYTHING", "41234"),
        ] {
            assert_eq!(poison(PG, var, value), None, "{var}");
        }
    }

    #[test]
    fn owner_pids_are_checked_without_truncation() {
        assert_eq!(owner_pid(1).unwrap(), 1);
        assert_eq!(owner_pid(MAX_OWNER_PID.into()).unwrap(), MAX_OWNER_PID);
        for bad in [
            0,
            u64::from(MAX_OWNER_PID) + 1,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
            u64::from(u32::MAX) + 2,
            u64::MAX,
        ] {
            assert_eq!(owner_pid(bad).unwrap_err().code, "usage", "{bad}");
        }
    }

    #[test]
    fn lease_expiry() {
        let lease = Lease {
            ttl_secs: Some(60),
            owner_pid: None,
            renewed_at: 1000,
        };
        assert!(lease.expired(1060).is_none());
        assert!(lease.expired(1061).is_some());

        let none = Lease {
            ttl_secs: None,
            owner_pid: None,
            renewed_at: 0,
        };
        assert!(none.expired(u64::MAX).is_none());

        let me = Lease {
            ttl_secs: None,
            owner_pid: Some(std::process::id()),
            renewed_at: 0,
        };
        assert!(
            me.expired(now()).is_none(),
            "a live owner keeps the session"
        );
        let gone = Lease {
            ttl_secs: None,
            owner_pid: Some(u32::MAX - 1),
            renewed_at: 0,
        };
        assert!(gone.expired(now()).unwrap().contains("exited"));
    }

    #[test]
    fn binding_vars_cover_preset_and_port_references() {
        let map = |pairs: &[(&str, &str)]| -> IndexMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let env = map(&[
            (
                "DATABASE_URL",
                "postgresql://postgres@127.0.0.1:41234/postgres",
            ),
            ("PGPORT", "41234"),
            ("PGHOSTADDR", "127.0.0.1"),
            ("APP_DB", "host=127.0.0.1:41234"),
            ("REDIS_URL", "redis://127.0.0.1:45555"),
            ("PATH", "/usr/bin"),
        ]);
        // Inherited by commands even though the provider does not set them.
        let inherited = map(&[
            ("PGSERVICE", "prod"),
            ("POSTGRES_HOST", "db.internal"),
            ("HOME", "/home/u"),
        ]);
        let mut service = Service {
            preset: Some("postgres".into()),
            version: None,
            run: None,
            ready_cmd: None,
            ready_port: None,
            port: None,
            identity: None,
            watch: Vec::new(),
            depends_on: Vec::new(),
            setup: None,
            init: None,
        };
        let vars = binding_vars("postgres", &service, Some(41234), &env, &inherited);
        for v in [
            "DATABASE_URL",
            "PGPORT",
            "PGHOST",
            "PGHOSTADDR",
            "PGSERVICE",
            "POSTGRES_HOST",
            "APP_DB",
        ] {
            assert!(vars.contains(&v.to_string()), "{v} not withheld: {vars:?}");
        }
        for v in ["REDIS_URL", "PATH", "HOME", "PGUSER"] {
            assert!(!vars.contains(&v.to_string()), "{v} withheld: {vars:?}");
        }

        service.preset = Some("redis".into());
        let vars = binding_vars("cache", &service, None, &env, &map(&[("CACHE_URL", "x")]));
        assert_eq!(vars, ["REDIS_URL", "CACHE_URL"]);
    }

    #[test]
    fn concurrently_keeps_the_order_of_its_items() {
        let items: Vec<u64> = (0..20).collect();
        // Later items finish first, so completion order differs from item order.
        let out = concurrently(&items, 4, |&i| {
            std::thread::sleep(Duration::from_millis(20 - i));
            i * 10
        });
        assert_eq!(out, items.iter().map(|i| i * 10).collect::<Vec<_>>());
    }

    #[test]
    fn concurrently_never_exceeds_its_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (running, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        concurrently(&[(); 9], 3, |_| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::yield_now();
            running.fetch_sub(1, Ordering::SeqCst);
        });
        let peak = peak.into_inner();
        // Scheduling may serialize the workers; only the upper bound is promised.
        assert!(peak <= 3, "peak concurrency {peak} exceeds limit 3");
    }

    #[test]
    fn only_a_launch_found_unchanged_is_saved_without_waiting_for_the_disk() {
        let record = |pid: u32, verified_at: u64| ServiceRecord {
            port: 20001,
            pid: Some(pid),
            data_dir: Some("/data/pg".into()),
            identity: Identity::Instance,
            verified_at,
            started_at_ms: Some(1),
            provider_id: None,
        };
        let session = |pid: u32, at: u64, ttl: Option<u64>| Session {
            id: "s".into(),
            services: IndexMap::from([("db".to_string(), record(pid, at))]),
            lease: ttl.map(|ttl| Lease { ttl_secs: Some(ttl), owner_pid: None, renewed_at: at }),
            ..Default::default()
        };
        let before = session(10, 100, Some(60));
        // Verified again and renewed later: the same launch.
        assert!(same_instance(&before, &session(10, 200, Some(60))));
        // Another process, another lease, another session, a launch in progress: not.
        assert!(!same_instance(&before, &session(11, 200, Some(60))));
        assert!(!same_instance(&before, &session(10, 200, Some(120))));
        assert!(!same_instance(&before, &session(10, 200, None)));
        assert!(!same_instance(&before, &Session { id: "t".into(), ..session(10, 200, Some(60)) }));
        assert!(!same_instance(&Session { launching: true, ..before.clone() }, &session(10, 200, Some(60))));
        let mut identity = session(10, 200, Some(60));
        identity.services["db"].identity = Identity::Liveness;
        assert!(!same_instance(&before, &identity));
    }

    #[test]
    #[should_panic(expected = "probe bug")]
    fn concurrently_propagates_a_worker_panic() {
        concurrently(&[1, 2, 3], 3, |&i| {
            if i == 2 {
                panic!("probe bug");
            }
        });
    }

    /// Environment whose PATH holds only a fake client named `bin` running `script`.
    #[cfg(unix)]
    fn fake_client(bin: &str, script: &str) -> (tempfile::TempDir, IndexMap<String, String>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bin);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:/bin:/usr/bin", dir.path().display());
        let env = IndexMap::from([("PATH".to_string(), path)]);
        (dir, env)
    }

    #[test]
    fn postgres_is_refused_up_front_only_for_root() {
        let services: IndexMap<String, Entry<Service>> = [("db", "preset = 'postgres'"), ("cache", "preset = 'redis'")]
            .into_iter()
            .map(|(n, t)| (n.to_string(), Entry { value: toml::from_str(t).unwrap(), origin: "project".into() }))
            .collect();
        let e = refused_as_root(0, &services).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), ("root_unsupported", "PostgreSQL (db) refuses to run as root, and Stack is running as root"));
        assert!(e.hint.unwrap().contains("su dev -c 'stack up'"));
        assert!(refused_as_root(1000, &services).is_ok());
        let redis: IndexMap<String, Entry<Service>> = services.into_iter().filter(|(n, _)| n == "cache").collect();
        assert!(refused_as_root(0, &redis).is_ok());
    }

    #[test]
    fn dependencies_start_first_and_restarting_one_restarts_what_depends_on_it() {
        let services: IndexMap<String, Entry<Service>> = [("worker", "depends_on = ['api']"), ("api", "depends_on = ['db', 'cache']"), ("db", ""), ("cache", ""), ("other", "")]
            .into_iter()
            .map(|(name, extra)| {
                let value: Service = toml::from_str(&format!("run = 'x'\n{extra}")).unwrap();
                (name.to_string(), Entry { value, origin: "project".into() })
            })
            .collect();
        let all: Vec<String> = services.keys().cloned().collect();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Waves by depth, each in declaration order; unrelated services start in the first.
        assert_eq!(dependency_waves(&services, &all), [names(&["db", "cache", "other"]), names(&["api"]), names(&["worker"])]);
        // Only dependencies among the services started count.
        assert_eq!(dependency_waves(&services, &names(&["worker", "db"])), [names(&["worker", "db"])]);
        assert_eq!(dependents_of(&services, &names(&["db"])), names(&["api", "worker"]));
        assert_eq!(dependents_of(&services, &names(&["api"])), names(&["worker"]));
        assert!(dependents_of(&services, &names(&["worker", "other"])).is_empty());
        // What a restart of only some services needs running first, through others too.
        assert_eq!(prerequisites_of(&services, &names(&["worker"])), names(&["api", "db", "cache"]));
        assert_eq!(prerequisites_of(&services, &names(&["worker", "api"])), names(&["db", "cache"]));
        assert!(prerequisites_of(&services, &all).is_empty());
        assert!(prerequisites_of(&services, &names(&["db", "other"])).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_postgres_cluster_that_is_not_utf8_is_warned_about_and_a_utf8_one_is_not() {
        let check = |data_dir: &str| Check {
            service: "postgres".into(),
            ready: true,
            port: Some(1),
            pid: None,
            identity: Some(Identity::Instance),
            reason: None,
            reason_code: None,
            withheld: Vec::new(),
            changed_since_start: Vec::new(),
            watch_incomplete: false,
            checked_at: 0,
            data_dir: Some(data_dir.into()),
            provider_id: None,
        };
        // A host name: not a URL Stack asks over the wire, so psql is asked (see `wire`).
        let (_dir, mut env) = fake_client("psql", r#"test "$1" = -X && test "$4" = 'show server_encoding' && echo SQL_ASCII"#);
        env.insert("DATABASE_URL".into(), "postgresql://postgres@localhost:1/postgres".into());
        let warning = encoding_warning(&env, &check("/data/pg")).expect("SQL_ASCII is warned about");
        assert!(warning.starts_with("postgres: the cluster in /data/pg uses encoding SQL_ASCII"), "{warning}");
        assert!(warning.contains("kept as it is") && warning.contains("C.UTF-8") && warning.contains("pg_dumpall"), "{warning}");
        let (_dir, mut env) = fake_client("psql", "echo UTF8");
        env.insert("DATABASE_URL".into(), "postgresql://x".into());
        assert_eq!(encoding_warning(&env, &check("/data/pg")), None);
        // A query that fails, or no URL, warns about nothing.
        let (_dir, mut env) = fake_client("psql", "exit 2");
        env.insert("DATABASE_URL".into(), "postgresql://x".into());
        assert_eq!(encoding_warning(&env, &check("/data/pg")), None);
        env.shift_remove("DATABASE_URL");
        assert_eq!(encoding_warning(&env, &check("/data/pg")), None);
    }

    /// A Postgres on a free port that reports `data_dir` for every query, to each connection
    /// (a bare TCP check included), until the test ends.
    fn wire_postgres(data_dir: &'static str) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in listener.incoming() {
                let Ok(mut c) = c else { continue };
                let msg = |kind: u8, body: &[u8]| [&[kind][..], &((body.len() + 4) as u32).to_be_bytes(), body].concat();
                let mut len = [0u8; 4];
                if c.read_exact(&mut len).is_err() {
                    continue;
                }
                let mut startup = vec![0u8; u32::from_be_bytes(len) as usize - 4];
                let _ = c.read_exact(&mut startup);
                let _ = c.write_all(&[msg(b'R', &[0, 0, 0, 0]), msg(b'Z', b"I")].concat());
                let mut head = [0u8; 5];
                if c.read_exact(&mut head).is_err() {
                    continue;
                }
                let mut query = vec![0u8; u32::from_be_bytes(head[1..].try_into().unwrap()) as usize - 4];
                let _ = c.read_exact(&mut query);
                let row = [&[0u8, 1][..], &(data_dir.len() as u32).to_be_bytes(), data_dir.as_bytes()].concat();
                let column = [&[0u8, 1][..], b"current_setting\0", &[0, 0, 0, 0, 0, 0, 0, 0, 0, 25, 255, 255, 255, 255, 255, 255, 0, 0]].concat();
                let _ = c.write_all(&[msg(b'T', &column), msg(b'D', &row), msg(b'C', b"SELECT 1\0"), msg(b'Z', b"I")].concat());
            }
        });
        port
    }

    #[test]
    fn an_answer_asked_over_the_wire_is_compared_as_the_clients_was() {
        if std::env::vars().any(|(k, _)| k.starts_with("PG") && !["PGHOST", "PGPORT", "PGUSER", "PGDATABASE"].contains(&k.as_str())) {
            eprintln!("skipping: a libpq variable in the environment leaves the question to psql");
            return;
        }
        let service = Service { preset: Some("postgres".into()), ..Default::default() };
        let status = |port: u16, dir: &str| DaemonStatus { id: None, name: "db".into(), status: "running".into(), pid: None, port: Some(port), data_dir: Some(dir.into()) };
        // No psql anywhere: the answer can only come over the wire.
        let env = |port: u16| IndexMap::from([("DATABASE_URL".to_string(), format!("postgresql://postgres@127.0.0.1:{port}/postgres")), ("PATH".to_string(), "/nonexistent".to_string())]);
        let root = Path::new("/");
        let port = wire_postgres("/data/this-checkout");
        assert_eq!(verify_one(root, &service, Some(port), Some(&status(port, "/data/this-checkout")), &env(port), None), Ok(Identity::Instance));
        let other = wire_postgres("/data/another-checkout");
        let (code, why) = verify_one(root, &service, Some(other), Some(&status(other, "/data/this-checkout")), &env(other), None).unwrap_err();
        assert_eq!(code, ReasonCode::WrongInstance);
        assert!(why.contains("data dir '/data/another-checkout', expected '/data/this-checkout'"), "{why}");
        // A URL Stack does not read itself goes to psql, run without ~/.psqlrc.
        let (_dir, mut with_psql) = fake_client("psql", r#"test "$1" = -X && echo /data/this-checkout"#);
        with_psql.insert("DATABASE_URL".into(), format!("postgresql://postgres@localhost:{port}/postgres"));
        assert_eq!(verify_one(root, &service, Some(port), Some(&status(port, "/data/this-checkout")), &with_psql, None), Ok(Identity::Instance));
        // ... and when psql is not there, it is never verified.
        let mut by_name = env(port);
        by_name.insert("DATABASE_URL".into(), format!("postgresql://postgres@localhost:{port}/postgres"));
        let (code, why) = verify_one(root, &service, Some(port), Some(&status(port, "/data/this-checkout")), &by_name, None).unwrap_err();
        assert_eq!((code, why.as_str()), (ReasonCode::ProbeFailed, "psql not found on the stack's PATH"));
    }

    #[cfg(unix)]
    #[test]
    fn run_probe_returns_client_output() {
        let (_dir, env) = fake_client("psql", r#"echo "$PGCONNECT_TIMEOUT $1""#);
        assert_eq!(run_probe(&env, "psql", &["arg"]).unwrap(), "3 arg\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_probe_reports_the_first_stderr_line_of_a_failed_client() {
        let (_dir, env) = fake_client(
            "psql",
            "echo 'psql: error: refused' >&2; echo more >&2; exit 2",
        );
        assert_eq!(
            run_probe(&env, "psql", &[]).unwrap_err(),
            "psql failed: psql: error: refused"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_probe_rejects_oversized_output_instead_of_reading_its_tail() {
        let (_dir, env) = fake_client(
            "redis-cli",
            "head -c 100000 /dev/zero; echo; echo /expected/dir",
        );
        let err = run_probe(&env, "redis-cli", &[]).unwrap_err();
        assert!(err.contains("printed more than"), "{err}");
    }

    /// Values a recorded path or id could hold, including every character a shell treats
    /// specially somewhere, and the empty string.
    const AWKWARD: &[&str] = &[
        "plain/path-1.2_x",
        "",
        "two words",
        "it's",
        "''",
        "\"double\"",
        "$HOME ${HOME} $(id) `id`",
        "a\\b\\",
        "line\nbreak\ttab",
        "*?[a-z]{1,2}",
        ";|&<>()!#%^",
        "~root",
        "=x",
        "-n",
        "ünïcødé ✓",
    ];

    #[cfg(unix)]
    #[test]
    fn shell_quoted_words_reach_a_command_unchanged() {
        for shell in ["sh", "bash", "zsh"] {
            if which_in(std::env::var("PATH").ok().as_deref(), shell).is_none() {
                continue;
            }
            for value in AWKWARD {
                let script = format!("printf '%s' {}", shell_quote(value));
                let out = Command::new(shell).args(["-c", &script]).output().unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), *value, "{shell}: {script}");
            }
        }
    }

    #[test]
    fn the_owner_mark_an_older_stack_wrote_is_read_and_a_replaced_record_is_never_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let path = state.join("supervisor-owner");
        assert_eq!(supervisor_owner(&state), Err(format!("{} does not exist", path.display())));
        assert!(!path.exists(), "Stack no longer writes it");
        fs::create_dir_all(&state).unwrap();
        let mark = format!("stack-{}", "0123456789abcdef0123456789abcdef");
        fs::write(&path, format!("{mark}\n")).unwrap();
        assert_eq!(supervisor_owner(&state).unwrap(), mark);
        for replaced in ["", "stack-\n", "stack-0123\n", &mark.to_uppercase(), &format!("{mark}x"), &format!("{mark}\n\n")] {
            fs::write(&path, replaced).unwrap();
            assert_eq!(supervisor_owner(&state), Err(format!("{} does not hold an owner mark Stack wrote", path.display())), "{replaced:?}");
            assert_eq!(fs::read_to_string(&path).unwrap(), replaced);
        }
    }

    #[cfg(unix)]
    #[test]
    fn one_supervisor_state_directory_has_one_lock_whatever_it_is_called() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("pf");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("alias")).unwrap();
        // In the supervisor's own state directory, so no Stack state has one of its own.
        let lock = supervisor_lock(&real.join("state"));
        assert_eq!(lock, fs::canonicalize(&real).unwrap().join("state/stack-supervisor.lock"));
        // Through a link, and before the directory exists.
        assert_eq!(supervisor_lock(&dir.path().join("alias/state")), lock);
        assert_eq!(resolved(&dir.path().join("alias/state/sock")), fs::canonicalize(&real).unwrap().join("state/sock"));
        assert_ne!(supervisor_lock(&dir.path().join("other/state")), lock);
    }

    #[test]
    fn pitchfork_commands_name_the_recorded_supervisor_and_id() {
        let provider = ProviderRecord { pitchfork: Some("/opt/pf/bin/pitchfork".into()), state_dir: "/tmp/pf state".into(), supervisor: None };
        assert_eq!(
            pitchfork_command(provider.legacy().unwrap(), &provider, "status", "app-0123/web"),
            "PITCHFORK_STATE_DIR='/tmp/pf state' /opt/pf/bin/pitchfork status -- app-0123/web"
        );
    }
}

#[cfg(test)]
mod native_records {
    use super::*;

    /// A session or data record Stack's own supervisor wrote still has the `pitchfork` field
    /// older releases require (empty, naming nothing), and reads back as Stack's.
    #[test]
    fn records_of_stacks_supervisor_stay_readable_by_older_releases() {
        let native = ProviderRecord::native("/s/supervisors/abc".into());
        let json = serde_json::to_value(&native).unwrap();
        assert_eq!(json, serde_json::json!({ "pitchfork": "", "state_dir": "/s/supervisors/abc", "supervisor": "stack" }));
        let back: ProviderRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, native);
        assert!(back.legacy().is_none());
        let old: ProviderRecord = serde_json::from_value(serde_json::json!({ "pitchfork": "/pf", "state_dir": "/st" })).unwrap();
        assert_eq!(old.legacy(), Some(Path::new("/pf")));
        let data = crate::datadirs::Supervisor { pitchfork: None, state_dir: "/d".into(), id: "db".into() };
        assert_eq!(serde_json::to_value(&data).unwrap()["pitchfork"], "");
    }

    /// Stack's supervisor confirms a recorded data directory's service stopped from its state
    /// file: a live process (by pid and start time) keeps the data.
    #[test]
    fn a_data_record_is_confirmed_stopped_from_the_supervisor_state() {
        let dir = tempfile::tempdir().unwrap();
        let me = crate::procinfo::process(std::process::id()).unwrap();
        let state = supervisor::State {
            supervisor: None,
            applied: 1,
            services: [(
                "db".to_string(),
                supervisor::Instance {
                    status: "running".into(),
                    process: Some(supervisor::Proc { pid: me.pid, started: me.started, boot: crate::procinfo::boot_id() }),
                    group: None,
                    port: 1,
                    data_dir: None,
                    stop_signal: libc::SIGTERM,
                    started_at_ms: 0,
                    exit: None,
                    restarts: 0,
                },
            )]
            .into_iter()
            .collect(),
            project: None,
            token: None,
        };
        fs::write(dir.path().join("state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        let record = crate::datadirs::Supervisor { pitchfork: None, state_dir: dir.path().into(), id: "db".into() };
        assert!(service_stopped(&record).unwrap_err().contains("reports db running"));
        // Nothing of the checkout still runs: confirmed stopped.
        let empty = tempfile::tempdir().unwrap();
        fs::write(empty.path().join("state.json"), serde_json::to_vec(&supervisor::State::default()).unwrap()).unwrap();
        let gone = crate::datadirs::Supervisor { state_dir: empty.path().into(), ..record };
        assert!(service_stopped(&gone).is_ok());
    }
}
