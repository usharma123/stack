//! Sessions: one running instance of a project on this machine.
//!
//! The session record says what stack started; it is never treated as proof of what is running.
//! Every `exec` and `status` re-checks each service against the supervisor and, where the service
//! type allows, confirms over the app's own connection settings that it reaches *this* instance.
//! Connection variables for a service that fails the check are withheld from commands.

use crate::error::{Result, StackError};
use crate::hash::sha256_hex;
use crate::manifest::Service;
use crate::ports;
use crate::project::{self, Options, Report};
use crate::provider::mise::{self, DaemonStatus};
use crate::source::Mode;
use crate::state::{now, pid_alive, project_key, project_lock, read_json, write_json};
use crate::timing::Timings;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(90);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Ctx {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
}

impl Ctx {
    fn compile(&self, write: bool) -> Result<Report> {
        project::compile_locked(&Options {
            root: self.root.clone(),
            mode: Mode::Frozen,
            write,
            cache: self.cache.clone(),
            state: self.state.clone(),
            reassign_ports: false,
            resolver: None,
        })
    }

    fn session_file(&self) -> PathBuf {
        self.root.join(".stack").join("session.json")
    }

    fn index_file(&self) -> PathBuf {
        index_path(&self.state, &self.root)
    }
}

fn index_path(state: &Path, root: &Path) -> PathBuf {
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
    /// Coordinators currently executing against this generation. Dead entries are ignored.
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// The Pitchfork binary mise used for this project.
    pub pitchfork: PathBuf,
    /// Pitchfork's effective state directory (socket and daemon state).
    pub state_dir: PathBuf,
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
    /// When stack last started this process (before the supervisor's start call). Older
    /// records lack it; then nothing is known about the files it loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
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
    /// Connection variables poisoned or removed for commands because the service is not verified.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub withheld: Vec<String>,
    /// Files under the service's `watch` paths modified after it started: the process may be
    /// running older code. Reported, not a verification failure.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_since_start: Vec<String>,
    pub checked_at: u64,
    #[serde(skip)]
    data_dir: Option<String>,
    #[serde(skip)]
    provider_id: Option<String>,
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
            withheld: Vec::new(),
            changed_since_start: Vec::new(),
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
            Err(reason) => {
                check.reason = Some(reason);
                check.withheld = binding_vars(name, service, port, env, &inherited);
            }
        }
        check
    };
    let services: Vec<(&String, &Service)> = report
        .stack
        .services
        .iter()
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
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
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
) -> std::result::Result<Identity, String> {
    let port = port.ok_or("no port assigned; run `stack compile`")?;
    let status = status.ok_or("not started")?;
    if status.status != "running" {
        return Err(format!("supervisor reports '{}'", status.status));
    }
    if let Some(pid) = status.pid {
        if !pid_alive(pid) {
            return Err(format!("process {pid} is not alive"));
        }
    }
    if let Some(actual) = status.port {
        if actual != port {
            return Err(format!("running on port {actual}, but this checkout is assigned {port}; run `stack down && stack up`"));
        }
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|_| format!("port {port} is not accepting connections"))?;

    let (var, bin) = match service.preset.as_deref() {
        Some("postgres") => ("DATABASE_URL", "psql"),
        Some("redis") => ("REDIS_URL", "redis-cli"),
        _ => {
            return match &service.identity {
                Some(probe) => {
                    let token = token.ok_or("no instance token assigned; run `stack compile`")?;
                    run_identity_probe(root, probe, env, token).map(|()| Identity::Instance)
                }
                None => Ok(Identity::Liveness),
            }
        }
    };
    let url = env.get(var).ok_or_else(|| format!("{var} is not set"))?;
    let expected = status
        .data_dir
        .as_deref()
        .ok_or("supervisor did not report a data directory")?;
    let args: Vec<&str> = match bin {
        "psql" => vec![url, "-Atc", "select current_setting('data_directory')"],
        _ => vec!["-u", url, "--no-auth-warning", "config", "get", "dir"],
    };
    let out = run_probe(env, bin, &args)?;
    let reported = out
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    if same_path(reported, expected) {
        Ok(Identity::Instance)
    } else {
        Err(format!(
            "{var} reaches a different server (data dir '{reported}', expected '{expected}')"
        ))
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

// ---- lifecycle -----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
struct Steps(Vec<Value>);

impl Steps {
    fn ok(&mut self, step: &str, detail: Value) {
        self.0
            .push(json!({ "step": step, "status": "ok", "detail": detail }));
    }

    /// Attach the steps that already ran, and whether repeating the command is safe.
    fn fail(mut self, step: &str, err: StackError, changed: bool) -> StackError {
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

#[derive(Debug, Default)]
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

#[derive(Debug, Serialize)]
pub struct UpReport {
    pub session: Session,
    pub checks: Vec<Check>,
    pub steps: Vec<Value>,
    pub reaped: Vec<GcEntry>,
}

/// Preserve every observed launch identity before further provider calls can fail.
fn record_launch_observations(ctx: &Ctx, statuses: &[DaemonStatus]) -> Result<()> {
    let Some(mut launch) = load(ctx)?.filter(|session| session.launching) else { return Ok(()) };
    for daemon in statuses {
        if let Some(record) = launch.services.get_mut(&daemon.name) {
            if daemon.port != Some(record.port) { continue; }
            record.provider_id = daemon.id.clone().or(record.provider_id.take());
            record.pid = daemon.pid.filter(|pid| (1..=MAX_OWNER_PID).contains(pid)).or(record.pid);
            record.data_dir = daemon.data_dir.clone().or(record.data_dir.take());
        }
    }
    save(ctx, &launch)
}

pub fn up(ctx: &Ctx, lease: LeaseOptions) -> Result<UpReport> {
    if let Some(pid) = lease.owner_pid {
        owner_pid(pid.into())?;
    }
    let mut steps = Steps::default();
    let reaped = gc(&ctx.state)?;
    steps.ok("gc", json!({ "reaped": reaped.len() }));

    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let previous = load(ctx)?;
    if previous.as_ref().is_some_and(has_active_executions) {
        return Err(steps.fail(
            "session",
            StackError::new(
                "session_busy",
                "commands are still executing in this session",
            ),
            false,
        ));
    }
    let report = match ctx.compile(true) {
        Ok(r) => r,
        Err(e) => return Err(steps.fail("compile", e, false)),
    };
    steps.ok("compile", json!({ "ports": report.ports }));

    if let Err(e) = mise::trust(&ctx.root) {
        return Err(steps.fail("install", e, false));
    }
    // Pitchfork refuses a socket path longer than `sun_path`, but only once it starts, after
    // every download. Check the path it will use first, with the env mise will give it.
    let mut socket = None;
    if !report.stack.services.is_empty() {
        match preflight_socket(ctx) {
            Ok((path, detail)) => {
                socket = path;
                steps.ok("preflight", detail);
            }
            Err(e) => return Err(steps.fail("preflight", e, false)),
        }
    }
    if let Err(e) = mise::install(&ctx.root) {
        return Err(steps.fail("install", e, false));
    }
    steps.ok("install", json!(null));
    // Recorded before anything starts, so a deleted project's services can still be found.
    let provider = socket.and_then(|socket| {
        let state_dir = socket.path.parent()?.parent()?.to_path_buf();
        Some(ProviderRecord { pitchfork: mise::which_pitchfork(&ctx.root)?, state_dir })
    });
    let project_dir_id = dir_id(&ctx.root);
    let project_dir_created = dir_created(&ctx.root);

    // `start` can reuse an already-running daemon with an old definition. Stop it first
    // when changing generations, or when no launch record establishes its configuration.
    let digest = config_digest(ctx, &report);
    let restart = previous.as_ref().is_some_and(|s| s.config_digest != digest);
    let unrecorded = previous.is_none() && !report.stack.services.is_empty();
    if restart || unrecorded {
        if let Err(e) = down_locked(ctx, provider.as_ref()) {
            return Err(steps.fail("stop_previous", e, true));
        }
        steps.ok("stop_previous", json!(null));
    }
    // Nothing can start on a port a foreign program holds, and the supervisor's own error
    // would come after the attempt. Stack's own daemons still running (an unchanged
    // generation) are not conflicts.
    if !report.stack.services.is_empty() {
        let statuses = match mise::daemons(&ctx.root) {
            Ok(s) => s,
            Err(e) => return Err(steps.fail("ports", e, false)),
        };
        let conflicts = port_conflicts(&report, &statuses);
        if !conflicts.is_empty() {
            return Err(steps.fail("ports", port_conflict_error(conflicts), false));
        }
        steps.ok("ports", json!(null));
    }

    let stamp = now();
    let launched_at = stamp;
    let lease = (lease.ttl_secs.is_some() || lease.owner_pid.is_some()).then_some(Lease {
        ttl_secs: lease.ttl_secs,
        owner_pid: lease.owner_pid,
        renewed_at: stamp,
    });
    let mut checks = Vec::new();
    if !report.stack.services.is_empty() {
        // Ownership is recorded before anything starts. If start or verification fails, or the
        // service is later removed from the configuration, `down` still knows what to reconcile.
        let unverified = previous.as_ref().map_or(true, |p| p.launching || p.config_digest != digest);
        if unverified {
            let launch = Session {
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
                        let record = ServiceRecord { port: *port, pid: None, data_dir: None, identity: Identity::Liveness, verified_at: 0, started_at: None, provider_id: None };
                        (name.clone(), record)
                    })
                    .collect(),
                provider: provider.clone(),
                project_dir_id,
                project_dir_created,
            };
            save(ctx, &launch).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        }
        // Capture qualified IDs before launch when available, including interrupted starts.
        let before = mise::daemons(&ctx.root).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        record_launch_observations(ctx, &before).map_err(|e| steps.clone().fail("record_launch", e, false))?;
        if let Err(start_error) = mise::start(&ctx.root) {
            let observed = mise::daemons(&ctx.root).and_then(|statuses| record_launch_observations(ctx, &statuses));
            if let Err(error) = observed {
                return Err(steps.fail("record_partial_start", error.with_detail(json!({ "start_error": start_error })), true));
            }
            return Err(steps.fail("start", start_error, true));
        }
        steps.ok("start", json!(null));

        checks = verify_until_ready(ctx, &report, "up_verify", |statuses| record_launch_observations(ctx, statuses))
            .map_err(|(step, e)| steps.clone().fail(step, e, true))?;
        steps.ok(
            "verify",
            json!(checks
                .iter()
                .map(|c| (c.service.clone(), c.identity))
                .collect::<IndexMap<_, _>>()),
        );
    }

    // Startup and verification are not idle time: the lease starts once services are verified.
    let stamp = now();
    let lease = lease.map(|l| Lease { renewed_at: stamp, ..l });
    // The same verified generation, still running: keep its identity so earlier callers'
    // records stay valid.
    let reused = previous
        .as_ref()
        .filter(|p| !p.launching && p.config_digest == digest && !restart && checks_match(p, &checks))
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
                // A process the previous record already knew keeps its start time; anything
                // else was started by this call.
                let kept = previous
                    .as_ref()
                    .and_then(|p| p.services.get(&c.service))
                    .filter(|r| r.pid.is_some() && r.pid == c.pid)
                    .map(|r| r.started_at.unwrap_or(r.verified_at));
                (c.service.clone(), record_of(c, kept.unwrap_or(launched_at)))
            })
            .collect(),
        provider: if report.stack.services.is_empty() { None } else { provider },
        project_dir_id,
        project_dir_created,
    };
    save(ctx, &session).map_err(|e| steps.clone().fail("record_session", e, true))?;
    Ok(UpReport {
        session,
        checks,
        steps: steps.0,
        reaped,
    })
}

/// Verify every service until all pass, or fail with `not_ready` after READY_TIMEOUT.
/// `observe` sees each supervisor listing before it is checked. Errors name their step.
fn verify_until_ready(
    ctx: &Ctx,
    report: &Report,
    timings: &'static str,
    mut observe: impl FnMut(&[DaemonStatus]) -> Result<()>,
) -> std::result::Result<Vec<Check>, (&'static str, StackError)> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let timings = Timings::new(timings);
    loop {
        let statuses = mise::daemons(&ctx.root).map_err(|e| ("verify", e))?;
        observe(&statuses).map_err(|e| ("record_observed", e))?;
        let env = mise::env(&ctx.root).map_err(|e| ("verify", e))?;
        let checks = verify_all(&ctx.root, report, &env, &statuses, &timings);
        if checks.iter().all(|c| c.ready) {
            return Ok(checks);
        }
        if Instant::now() >= deadline {
            let failed: Vec<Value> = checks
                .iter()
                .filter(|c| !c.ready)
                .map(|c| json!({ "service": c.service, "reason": c.reason }))
                .collect();
            let err = StackError::new("not_ready", format!("{} service(s) failed verification", failed.len()))
                .details(failed);
            return Err(("verify", err));
        }
        sleep(Duration::from_millis(300));
    }
}

#[derive(Debug, Serialize)]
pub struct RestartReport {
    pub session: Session,
    pub restarted: Vec<String>,
    pub checks: Vec<Check>,
    pub steps: Vec<Value>,
}

/// Stop and start the named services (every service when none are named) of the running,
/// current session, then verify the whole stack again. Other services keep running. The
/// session keeps its id: the configuration is unchanged, only processes are new.
pub fn restart(ctx: &Ctx, services: &[String]) -> Result<RestartReport> {
    let mut steps = Steps::default();
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?.ok_or_else(|| {
        StackError::new("no_session", "no session for this project").hint("run `stack up`")
    })?;
    if has_active_executions(&session) {
        return Err(StackError::new("session_busy", "commands are still executing in this session"));
    }
    let report = ctx.compile(true)?;
    mise::trust(&ctx.root)?;
    if session.launching || session.config_digest != config_digest(ctx, &report) {
        return Err(StackError::new(
            "session_stale",
            "the running services were not verified with the current configuration",
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
    if names.is_empty() {
        names = report.stack.services.keys().cloned().collect();
    }
    if names.is_empty() {
        return Err(StackError::new("unknown_service", "this project defines no services"));
    }

    // Confirm the old processes are gone before starting new ones on the same ports.
    if let Err(e) = mise::stop_daemons(&ctx.root, &names) {
        return Err(steps.fail("stop", e, true));
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let alive: Vec<Value> = names
            .iter()
            .filter_map(|n| session.services.get(n).map(|r| (n, r)))
            .filter(|(_, r)| r.pid.is_some_and(pid_alive) || accepting(r.port))
            .map(|(n, r)| json!({ "service": n, "pid": r.pid, "port": r.port }))
            .collect();
        if alive.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            let err = StackError::new("stop_unconfirmed", "services did not stop")
                .hint("retry `stack restart`, or `stack down` and `stack up`")
                .details(alive);
            return Err(steps.fail("stop", err, true));
        }
        sleep(Duration::from_millis(200));
    }
    steps.ok("stop", json!(names));

    // The supervisor stamps output with whole seconds. Starting in a second the old processes
    // never wrote in keeps `logs --since-start` free of their last lines.
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    sleep(Duration::from_secs(elapsed.as_secs() + 1) - elapsed);
    let started_at = now();
    if let Err(e) = mise::start_daemons(&ctx.root, &names) {
        return Err(steps.fail("start", e, true));
    }
    steps.ok("start", json!(names));
    let checks = verify_until_ready(ctx, &report, "restart_verify", |_| Ok(()))
        .map_err(|(step, e)| steps.clone().fail(step, e, true))?;
    steps.ok("verify", json!(checks.iter().map(|c| (c.service.clone(), c.identity)).collect::<IndexMap<_, _>>()));

    for check in &checks {
        // Untouched services keep their start time while they are the same process.
        let started = session
            .services
            .get(&check.service)
            .filter(|r| !names.contains(&check.service) && r.pid.is_some() && r.pid == check.pid)
            .and_then(|r| r.started_at)
            .unwrap_or(started_at);
        session.services.insert(check.service.clone(), record_of(check, started));
    }
    if let Some(lease) = session.lease.as_mut() {
        lease.renewed_at = now();
    }
    save(ctx, &session).map_err(|e| steps.clone().fail("record_session", e, true))?;
    Ok(RestartReport { session, restarted: names, checks, steps: steps.0 })
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub steps: Vec<Value>,
    pub ports: IndexMap<String, u16>,
    pub versions: Vec<project::VersionReport>,
}

/// Install every locked tool and service binary without starting anything or recording a
/// session: the compile, trust, socket preflight and install steps of `up`, and nothing after.
/// Locked like `up`: a missing or stale pin is `lock_outdated`, never resolved here.
pub fn install(ctx: &Ctx) -> Result<InstallReport> {
    let mut steps = Steps::default();
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let report = match ctx.compile(true) {
        Ok(r) => r,
        Err(e) => return Err(steps.fail("compile", e, false)),
    };
    steps.ok("compile", json!({ "ports": report.ports }));
    if let Err(e) = mise::trust(&ctx.root) {
        return Err(steps.fail("install", e, false));
    }
    if !report.stack.services.is_empty() {
        match preflight_socket(ctx) {
            Ok((_, detail)) => steps.ok("preflight", detail),
            Err(e) => return Err(steps.fail("preflight", e, false)),
        }
    }
    if let Err(e) = mise::install(&ctx.root) {
        return Err(steps.fail("install", e, false));
    }
    steps.ok("install", json!(null));
    Ok(InstallReport { steps: steps.0, ports: report.ports, versions: report.versions })
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
    let report = ctx.compile(false)?;
    if !report.stack.services.contains_key(service) {
        return Err(unknown_service(&report, service));
    }
    let (configured, started_at) = {
        let _guard = project_lock(&ctx.state, &ctx.root)?;
        let session = load(ctx)?;
        let started_at = session.as_ref().and_then(|s| s.services.get(service)).and_then(|r| r.started_at);
        (has_provider_config(ctx, &report, session.is_some())?, started_at)
    };
    if !configured {
        return Err(StackError::new("logs_failed", format!("no launch record for {service} in this checkout"))
            .hint("run `stack up` first"));
    }
    let since = match (since_start, started_at) {
        (false, _) => None,
        (true, Some(at)) => Some(at),
        (true, None) => {
            return Err(StackError::new("logs_failed", format!("no recorded start time for {service}"))
                .hint("sessions started before stack 0.1.18 lack one; `stack restart` records it"))
        }
    };
    let out = mise::logs(&ctx.root, service, tail, since)?;
    let lines: Vec<String> = out.stdout.lines().map(str::to_string).collect();
    let skip = lines.len().saturating_sub(tail);
    Ok(LogsReport {
        service: service.to_string(),
        lines: lines[skip..].to_vec(),
        truncated: out.stdout_truncated,
        started_at,
        since_start,
    })
}

/// `unknown_service`, naming the services the project does define.
fn unknown_service(report: &Report, name: &str) -> StackError {
    let known: Vec<&str> = report.stack.services.keys().map(String::as_str).collect();
    StackError::new("unknown_service", format!("no service named '{name}'")).hint(if known.is_empty() {
        "this project defines no services".to_string()
    } else {
        format!("services: {}", known.join(", "))
    })
}

/// The supervisor socket Pitchfork will use for this project, or `socket_path_too_long`.
fn preflight_socket(ctx: &Ctx) -> Result<(Option<mise::SocketPath>, Value)> {
    let effective = mise::env(&ctx.root)?;
    let env = mise::SocketEnv::effective(&ctx.root, &effective);
    match mise::socket_path(&env, mise::socket_capacity()) {
        Ok(socket) if socket.fits() => {
            let detail = json!({ "socket": socket });
            Ok((Some(socket), detail))
        }
        Ok(socket) => Err(socket.error()),
        Err(note) => Ok((None, json!({ "socket": null, "note": note }))),
    }
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

fn dir_created(path: &Path) -> Option<std::time::SystemTime> {
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
fn record_of(check: &Check, started_at: u64) -> ServiceRecord {
    ServiceRecord {
        port: check.port.unwrap_or_default(),
        pid: check.pid,
        data_dir: check.data_dir.clone(),
        identity: check.identity.unwrap_or(Identity::Liveness),
        verified_at: check.checked_at,
        started_at: Some(started_at),
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
pub fn down(ctx: &Ctx) -> Result<DownReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    down_locked(ctx, None)
}

fn down_locked(ctx: &Ctx, provider: Option<&ProviderRecord>) -> Result<DownReport> {
    let session = load(ctx)?;
    let reserved = ports::lookup(&ctx.state, &ctx.root)?;
    // Services always hold a port reservation from compile, and a launch record names any
    // process stack started. With neither, stack owns nothing and the supervisor (which may
    // not even be configured for a tools-only project) has nothing to report.
    let owns_services = !reserved.is_empty() || session.as_ref().is_some_and(|s| !s.services.is_empty());
    // Failure to discover ownership cannot establish that nothing is running.
    let before = if owns_services { mise::daemons(&ctx.root)? } else { Vec::new() };
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

    let provider = session.as_ref().and_then(|s| s.provider.clone()).or_else(|| provider.cloned());
    let mut ports = owned_ports(ctx, session.as_ref(), &before, provider.as_ref());
    ports.sort_unstable();
    ports.dedup();
    ports.retain(|p| *p != 0);

    // Providers may list configured but never-started daemons. Stopping those returns
    // "no matching daemons"; only invoke stop when there is actual ownership to reconcile.
    let needs_stop = pids.iter().any(|(_, pid)| pid_alive(*pid))
        || before.iter().any(|d| matches!(d.status.as_str(), "running" | "starting"));
    if needs_stop {
        mise::stop(&ctx.root)?;
    }

    let deadline = Instant::now() + STOP_TIMEOUT;
    let leftovers = loop {
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

    remove_if_exists(&ctx.session_file())?;
    remove_if_exists(&ctx.index_file())?;
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
            .collect(),
        confirmed: true,
        conflicts,
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
fn owned_ports(
    ctx: &Ctx,
    session: Option<&Session>,
    statuses: &[DaemonStatus],
    provider: Option<&ProviderRecord>,
) -> Vec<u16> {
    let mut discovered: Option<Option<ProviderRecord>> = None;
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
        let provider = match provider {
            Some(p) => Some(p.clone()),
            None => discovered.get_or_insert_with(|| discover_provider(ctx)).clone(),
        };
        let active = match (&d.id, &provider) {
            (Some(id), Some(p)) => match mise::supervised(&p.pitchfork, &p.state_dir, id) {
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

/// The supervisor as `up` would record it, for a project without a launch record.
fn discover_provider(ctx: &Ctx) -> Option<ProviderRecord> {
    let (socket, _) = preflight_socket(ctx).ok()?;
    let state_dir = socket?.path.parent()?.parent()?.to_path_buf();
    Some(ProviderRecord { pitchfork: mise::which_pitchfork(&ctx.root)?, state_dir })
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
        hints.push("run `stack compile --reassign-ports` to give this checkout other ports, then `stack up`".to_string());
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

fn accepting(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(200),
    )
    .is_ok()
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub session: Option<Session>,
    pub checks: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expired: Option<String>,
    /// The complete compiled configuration changed since the session started.
    pub stale: bool,
    /// Every service verified and the session is current; `stack status` exits 1 otherwise.
    pub healthy: bool,
}

/// Always answers, even for a broken session: diagnosing that state is the point.
pub fn status(ctx: &Ctx) -> Result<StatusReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let session = load(ctx)?;
    let report = ctx.compile(false)?;
    let checks = if report.stack.services.is_empty() {
        Vec::new()
    } else if !has_provider_config(ctx, &report, session.is_some())? {
        not_launched(&report)
    } else {
        let (env, statuses) = mise::env_and_daemons(&ctx.root)?;
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
        .is_some_and(|s| s.config_digest != config_digest(ctx, &report));
    Ok(StatusReport {
        healthy: !stale && checks.iter().all(|c| c.ready),
        stale,
        lease_expired: session
            .as_ref()
            .and_then(|s| s.lease.as_ref())
            .and_then(|l| l.expired(now())),
        session,
        checks,
    })
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
            reason: Some("no launch record in this checkout; run `stack up`".into()),
            withheld: Vec::new(),
            changed_since_start: Vec::new(),
            checked_at: now(),
            data_dir: None,
            provider_id: None,
        })
        .collect()
}

pub struct ExecPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Complete environment for the command, including poisoned endpoints.
    pub env: IndexMap<String, String>,
    /// Variables that must not be inherited from the caller either.
    pub removed: Vec<String>,
    pub checks: Vec<Check>,
    /// Keeps TTL collection from stopping services until the command finishes.
    pub execution: Option<ExecutionGuard>,
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

/// Which services a command needs. Unverified services are always withheld; required ones
/// also make the command fail instead of running.
pub enum Require {
    Nothing,
    All,
    Only(Vec<String>),
}

/// A declared task as a command for `plan_exec`, with every service required. `mise run`
/// keeps the provider's task semantics (templates, shebangs, argument passing); `--skip-deps`
/// stops it from starting the task's daemons itself, since stack verifies them instead.
pub fn task_command(ctx: &Ctx, name: &str, args: &[String]) -> Result<(Vec<String>, Require)> {
    let report = ctx.compile(false)?;
    report.stack.tasks.get(name).ok_or_else(|| {
        let known: Vec<&str> = report.stack.tasks.keys().map(String::as_str).collect();
        StackError::new("unknown_task", format!("no task named '{name}'")).hint(if known.is_empty() {
            "this project defines no tasks; add one under [tasks.<name>] in stack.toml".to_string()
        } else {
            format!("tasks: {}", known.join(", "))
        })
    })?;
    let mut command: Vec<String> = ["mise", "run", "--skip-deps", "--no-timings", name, "--"]
        .into_iter()
        .map(String::from)
        .collect();
    command.extend(args.iter().cloned());
    // `mise run` evaluates the provider config again and would restore the endpoints that
    // `plan_exec` withholds from unverified services. Only a fully verified stack has none.
    Ok((command, Require::All))
}

/// Prepare a command: verify services, withhold unverified endpoints, renew the lease.
pub fn plan_exec(ctx: &Ctx, cmd: &[String], require: &Require) -> Result<ExecPlan> {
    let mut timings = Timings::new("exec");
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    timings.mark("lock");
    let mut session = load(ctx)?;
    let report = ctx.compile(true)?;
    timings.mark("compile");
    mise::trust(&ctx.root)?;
    timings.mark("trust");
    let (mut env, checks) = if report.stack.services.is_empty() {
        let env = mise::env(&ctx.root)?;
        timings.mark("env");
        (env, Vec::new())
    } else {
        let (env, statuses) = mise::env_and_daemons(&ctx.root)?;
        timings.mark("env_daemons");
        let checks = verify_session(ctx, &report, &env, &statuses, session.as_ref(), &timings);
        timings.mark("verify");
        (env, checks)
    };

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
    if let Some(session) = session
        .as_mut()
        .filter(|s| !s.launching && s.config_digest == config_digest(ctx, &report))
    {
        if let Some(lease) = session.lease.as_mut() {
            lease.renewed_at = now();
        }
        let token = sha256_hex(format!("{:?}{}", Instant::now(), std::process::id()).as_bytes());
        session.active_executions.retain(|_, pid| pid_alive(*pid));
        session
            .active_executions
            .insert(token.clone(), std::process::id());
        if let Err(e) = save(ctx, session) {
            // A failed project-mirror write must not leave a phantom execution attached
            // to a long-lived MCP coordinator in the authoritative machine index.
            session.active_executions.shift_remove(&token);
            let _ = write_json(&ctx.index_file(), session);
            return Err(e);
        }
        execution = Some(ExecutionGuard {
            root: ctx.root.clone(),
            state: ctx.state.clone(),
            session_id: session.id.clone(),
            token,
        });
        env.insert("STACK_SESSION".into(), session.id.clone());
    } else {
        env.shift_remove("STACK_SESSION");
        removed.push("STACK_SESSION".into());
    }
    env.insert("STACK_PROJECT".into(), ctx.root.to_string_lossy().into());
    timings.mark("reserve");

    Ok(ExecPlan {
        program,
        args: args.to_vec(),
        env,
        removed,
        checks,
        execution,
    })
}

pub fn renew(ctx: &Ctx) -> Result<Session> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?.ok_or_else(|| {
        StackError::new("no_session", "no session for this project").hint("run `stack up`")
    })?;
    let lease = session.lease.as_mut().ok_or_else(|| {
        StackError::new(
            "no_lease",
            "this session has no lease; it lives until `stack down`",
        )
    })?;
    lease.renewed_at = now();
    save(ctx, &session)?;
    Ok(session)
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
    pub services: Vec<Value>,
}

/// Reclaim sessions whose lease expired or whose project directory is gone.
pub fn gc(state: &Path) -> Result<Vec<GcEntry>> {
    let dir = state.join("sessions");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let indexed: Session = read_json(&path)?;
        let ctx = Ctx {
            root: indexed.project.clone(),
            cache: PathBuf::new(),
            state: state.to_path_buf(),
        };
        let _guard = project_lock(state, &ctx.root)?;
        // Re-read after locking: another command may have renewed or replaced this generation.
        if !path.exists() {
            continue;
        }
        let session: Session = read_json(&path)?;
        // TTL describes idle time; an executing command is not idle. Owner death still
        // follows the explicit runner policy even if one of its commands has survived.
        let owner_dead = session
            .lease
            .as_ref()
            .and_then(|l| l.owner_pid)
            .is_some_and(|p| !pid_alive(p));
        let busy = !owner_dead && has_active_executions(&session);
        if let Some(reason) = project_gone(&session) {
            if !busy {
                out.push(reclaim_gone(&path, session, reason));
            }
            continue;
        }
        let Some(reason) = session.lease.as_ref().and_then(|l| l.expired(now())) else {
            continue;
        };
        if busy {
            continue;
        }
        let result = down_locked(&ctx, None);
        out.push(GcEntry {
            project: session.project,
            reason,
            stopped: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
            services: Vec::new(),
        });
    }
    Ok(out)
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
fn reclaim_gone(index: &Path, session: Session, reason: &str) -> GcEntry {
    let mut services = Vec::new();
    let mut problems = Vec::new();
    for (name, record) in &session.services {
        match reconcile_gone(session.provider.as_ref(), record, session.launching) {
            Ok(outcome) => services.push(json!({ "service": name, "outcome": outcome })),
            Err(why) => {
                services.push(json!({ "service": name, "error": why }));
                problems.push(format!("{name}: {why}"));
            }
        }
    }
    if problems.is_empty() {
        if let Err(e) = remove_if_exists(index) {
            problems.push(e.to_string());
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

fn reconcile_gone(provider: Option<&ProviderRecord>, record: &ServiceRecord, launching: bool) -> std::result::Result<String, String> {
    let alive = record.pid.is_some_and(pid_alive);
    let listening = record.port != 0 && accepting(record.port);
    let (Some(provider), Some(id)) = (provider, record.provider_id.as_deref()) else {
        return Err(match record.pid {
            Some(pid) if alive => format!("pid {pid} is alive, but no supervisor id was recorded to confirm it is this service; not signalled"),
            _ => "no supervisor identity was recorded; terminal state cannot be confirmed; record retained".into(),
        });
    };
    match mise::supervised(&provider.pitchfork, &provider.state_dir, id)? {
        mise::Supervised::NotFound => match record.pid {
            _ if launching => Err("startup was interrupted or failed; absence of a daemon now does not prove that startup cannot still register it; record retained".into()),
            Some(pid) if alive => Err(format!("the supervisor no longer tracks {id}, and pid {pid} is alive (possibly reused); not signalled")),
            _ if listening => Ok(format!("port {} is held by a process the supervisor does not track; left alone", record.port)),
            _ => Ok("not running".into()),
        },
        mise::Supervised::Found { status, pid, port } => {
            if status == "stopped" && !alive && !pid.is_some_and(pid_alive) && !launching {
                return Ok("not running (supervisor confirms stopped)".into());
            }
            if status != "running" || !pid.is_some_and(pid_alive) {
                return Err(format!("supervisor state {status:?} is not confirmed terminal cleanup for {id}; record retained"));
            }
            let Some(recorded) = record.pid else {
                return Err(format!("{id} is running as pid {}, but no verified pid was recorded for it; not signalled", pid.unwrap_or(0)));
            };
            if pid != Some(recorded) {
                return Err(format!(
                    "{id} now runs pid {}, not the recorded pid {recorded} (restarted, or the path was reused); not signalled",
                    pid.unwrap_or(0)
                ));
            }
            if port != Some(record.port) {
                return Err(format!("{id} now uses port {}, not the recorded port {}; not signalled", port.unwrap_or(0), record.port));
            }
            Err(format!("{id} matches pid {recorded}, but Pitchfork cannot atomically validate and stop that generation; not signalled. Stop the service explicitly and retry GC"))
        }
    }
}

/// Like [`gc`], but fails when any session it tried to reclaim is still running or could not
/// be confirmed stopped. Ownership records are kept for a retry.
pub fn gc_checked(state: &Path) -> Result<Vec<GcEntry>> {
    let entries = gc(state)?;
    let failed = entries.iter().filter(|e| !e.stopped).count();
    if failed == 0 {
        return Ok(entries);
    }
    Err(StackError::new(
        "gc_incomplete",
        format!("{failed} session(s) could not be confirmed stopped"),
    )
    .hint("retry `stack gc`; for a live project, `stack down` in it. Records are kept until cleanup is confirmed")
    .details(entries.iter().map(|e| serde_json::to_value(e).expect("gc entry serializes")).collect()))
}

fn load(ctx: &Ctx) -> Result<Option<Session>> {
    // The machine index is authoritative. A crash between the two atomic writes can
    // leave the project copy behind; lifecycle operations always use the indexed generation.
    let indexed = ctx.index_file().exists();
    let path = if indexed { ctx.index_file() } else { ctx.session_file() };
    if !path.exists() {
        return Ok(None);
    }
    let session = read_json::<Session>(&path)?;
    // The project copy mirrors the record for this path only. One naming another directory
    // was copied in with the files (committed to Git, a cloned or duplicated checkout): its
    // services belong to that directory, whose own record is in the machine index.
    if !indexed && session.project != ctx.root {
        return Ok(None);
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

fn save(ctx: &Ctx, session: &Session) -> Result<()> {
    write_json(&ctx.index_file(), session)?;
    write_json(&ctx.session_file(), session)?;
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

fn has_active_executions(session: &Session) -> bool {
    session
        .active_executions
        .values()
        .any(|pid| pid_alive(*pid))
}

/// A note when this checkout's running session was started from a configuration other than
/// `report` (such as an edited `[env]`), for `inspect`. Read-only: no lock, no provider calls.
pub fn stale_session(ctx: &Ctx, report: &Report) -> Option<String> {
    let session = load(ctx).ok()??;
    (session.config_digest != config_digest(ctx, report)).then(|| {
        "the running services were started from a different configuration; `stack up` restarts them with this one".to_string()
    })
}

fn config_digest(ctx: &Ctx, report: &Report) -> String {
    let mut config =
        json!({ "lock": lock_digest(&ctx.root), "stack": report.stack, "ports": report.ports });
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
        None => Some("no launch record; run `stack up`"),
        Some(s) if s.launching => Some("`stack up` did not finish verifying this launch; run `stack up`"),
        Some(s) if s.config_digest != config_digest(ctx, report) => {
            Some("session configuration changed; run `stack up` to restart and verify it")
        }
        _ => None,
    };
    for check in &mut checks {
        let changed_process = session
            .and_then(|s| s.services.get(&check.service))
            .is_some_and(|r| r.pid != check.pid);
        if let Some(reason) = reason
            .or(changed_process.then_some("service process changed since launch; run `stack up`"))
        {
            check.ready = false;
            check.identity = None;
            check.reason = Some(reason.into());
            check.withheld = binding_vars(
                &check.service,
                &report.stack.services[&check.service].value,
                check.port,
                env,
                &inherited_env(),
            );
        }
        let watch = &report.stack.services[&check.service].value.watch;
        let started = session.and_then(|s| s.services.get(&check.service)).and_then(|r| r.started_at);
        if let (false, Some(started)) = (watch.is_empty(), started) {
            check.changed_since_start = changed_since(&ctx.root, watch, started);
        }
    }
    checks
}

/// Most changed files a check names; the first are enough to act on.
const CHANGED_LIMIT: usize = 5;
/// Most directory entries examined per check, so a huge tree cannot stall every command.
const WATCH_SCAN_LIMIT: usize = 20_000;

/// Files under `watch` (relative to `root`) modified after `started_at`, in the order found.
/// Directories are walked recursively, skipping hidden entries and common build and
/// dependency directories.
fn changed_since(root: &Path, watch: &[String], started_at: u64) -> Vec<String> {
    let modified_after = |meta: &fs::Metadata| {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() > started_at)
    };
    let mut changed = Vec::new();
    let mut pending: Vec<PathBuf> = watch.iter().map(|w| root.join(w)).collect();
    pending.reverse();
    let mut scanned = 0;
    while let Some(path) = pending.pop() {
        if changed.len() >= CHANGED_LIMIT || scanned >= WATCH_SCAN_LIMIT {
            break;
        }
        scanned += 1;
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else { continue };
            let mut children: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    !name.starts_with('.') && !matches!(name.as_ref(), "node_modules" | "target" | "__pycache__")
                })
                .map(|e| e.path())
                .collect();
            children.sort();
            children.reverse();
            pending.extend(children);
        } else if modified_after(&meta) {
            let shown = path.strip_prefix(root).unwrap_or(&path);
            changed.push(shown.display().to_string());
        }
    }
    changed
}

/// An execution is registered under the lifecycle lock and removed at command completion.
/// A crashed coordinator is ignored by GC through its PID, without a background heartbeat.
pub struct ExecutionGuard {
    root: PathBuf,
    state: PathBuf,
    session_id: String,
    token: String,
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        let _timings = Timings::new("release");
        let ctx = Ctx {
            root: self.root.clone(),
            cache: PathBuf::new(),
            state: self.state.clone(),
        };
        let Ok(_guard) = project_lock(&ctx.state, &ctx.root) else {
            return;
        };
        let Ok(Some(mut session)) = load(&ctx) else {
            return;
        };
        if session.id != self.session_id {
            return;
        }
        session.active_executions.shift_remove(&self.token);
        if let Some(lease) = session.lease.as_mut() {
            lease.renewed_at = now();
        }
        if let Err(e) = save(&ctx, &session) {
            eprintln!("stack: cannot finish execution lease: {e}");
        }
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
}
