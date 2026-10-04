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
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    pub services: IndexMap<String, ServiceRecord>,
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
    pub checked_at: u64,
    #[serde(skip)]
    data_dir: Option<String>,
}

// ---- verification --------------------------------------------------------------------------

fn verify_all(
    report: &Report,
    env: &IndexMap<String, String>,
    statuses: &[DaemonStatus],
) -> Vec<Check> {
    report
        .stack
        .services
        .iter()
        .map(|(name, entry)| {
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
                checked_at: now(),
                data_dir: status.and_then(|s| s.data_dir.clone()),
            };
            match verify_one(&entry.value, port, status, env) {
                Ok(identity) => {
                    check.ready = true;
                    check.identity = Some(identity);
                }
                Err(reason) => {
                    check.reason = Some(reason);
                    check.withheld = binding_vars(name, &entry.value, port, env);
                }
            }
            check
        })
        .collect()
}

fn verify_one(
    service: &Service,
    port: Option<u16>,
    status: Option<&DaemonStatus>,
    env: &IndexMap<String, String>,
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
        _ => return Ok(Identity::Liveness),
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

fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    !a.is_empty() && canon(a) == canon(b)
}

/// Run a client binary from the stack's PATH with a hard deadline.
fn run_probe(
    env: &IndexMap<String, String>,
    bin: &str,
    args: &[&str],
) -> std::result::Result<String, String> {
    let path = which_in(env.get("PATH").map(String::as_str), bin)
        .ok_or_else(|| format!("{bin} not found on the stack's PATH"))?;
    let mut child = Command::new(path)
        .args(args)
        .envs(env)
        .env("PGCONNECT_TIMEOUT", "3")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                return Err(format!(
                    "{bin} did not answer within {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{bin}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{bin} failed: {}",
            err.lines().next().unwrap_or("").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn which_in(path: Option<&str>, bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        return Some(PathBuf::from(bin));
    }
    std::env::split_paths(path?)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Variables that point a command at this service.
fn binding_vars(
    name: &str,
    service: &Service,
    port: Option<u16>,
    env: &IndexMap<String, String>,
) -> Vec<String> {
    let mut vars: Vec<String> = match service.preset.as_deref() {
        Some("postgres") => [
            "DATABASE_URL",
            "PGHOST",
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
        if env.contains_key(&v) && !out.contains(&v) {
            out.push(v);
        }
    }
    out
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

#[derive(Debug, Serialize)]
pub struct UpReport {
    pub session: Session,
    pub checks: Vec<Check>,
    pub steps: Vec<Value>,
    pub reaped: Vec<GcEntry>,
}

pub fn up(ctx: &Ctx, lease: LeaseOptions) -> Result<UpReport> {
    let mut steps = Steps::default();
    let reaped = gc(&ctx.state)?;
    steps.ok("gc", json!({ "reaped": reaped.len() }));

    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let previous = load(ctx)?;
    let report = match ctx.compile(true) {
        Ok(r) => r,
        Err(e) => return Err(steps.fail("compile", e, false)),
    };
    steps.ok("compile", json!({ "ports": report.ports }));

    if let Err(e) = mise::trust(&ctx.root).and_then(|_| mise::install(&ctx.root)) {
        return Err(steps.fail("install", e, false));
    }
    steps.ok("install", json!(null));

    // `start` can reuse an already-running daemon with an old definition. Stop it first
    // when changing generations, or when no launch record establishes its configuration.
    let digest = config_digest(ctx, &report);
    let restart = previous.as_ref().is_some_and(|s| s.config_digest != digest);
    let unrecorded = previous.is_none() && !report.stack.services.is_empty();
    if restart || unrecorded {
        if let Err(e) = down_locked(ctx) {
            return Err(steps.fail("stop_previous", e, true));
        }
        steps.ok("stop_previous", json!(null));
    }

    let mut checks = Vec::new();
    if !report.stack.services.is_empty() {
        if let Err(e) = mise::start(&ctx.root) {
            return Err(steps.fail("start", e, true));
        }
        steps.ok("start", json!(null));

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let env = mise::env(&ctx.root).map_err(|e| steps.clone().fail("verify", e, true))?;
            let statuses =
                mise::daemons(&ctx.root).map_err(|e| steps.clone().fail("verify", e, true))?;
            checks = verify_all(&report, &env, &statuses);
            if checks.iter().all(|c| c.ready) {
                break;
            }
            if Instant::now() >= deadline {
                let failed: Vec<Value> = checks
                    .iter()
                    .filter(|c| !c.ready)
                    .map(|c| json!({ "service": c.service, "reason": c.reason }))
                    .collect();
                let err = StackError::new(
                    "not_ready",
                    format!("{} service(s) failed verification", failed.len()),
                )
                .details(failed);
                return Err(steps.fail("verify", err, true));
            }
            sleep(Duration::from_millis(300));
        }
        steps.ok(
            "verify",
            json!(checks
                .iter()
                .map(|c| (c.service.clone(), c.identity))
                .collect::<IndexMap<_, _>>()),
        );
    }

    let stamp = now();
    let session = Session {
        id: sha256_hex(
            format!(
                "{}{:?}{}",
                ctx.root.display(),
                Instant::now(),
                std::process::id()
            )
            .as_bytes(),
        )[..12]
            .to_string(),
        project: ctx.root.clone(),
        lock_digest: lock_digest(&ctx.root),
        config_digest: digest,
        started_at: stamp,
        lease: (lease.ttl_secs.is_some() || lease.owner_pid.is_some()).then_some(Lease {
            ttl_secs: lease.ttl_secs,
            owner_pid: lease.owner_pid,
            renewed_at: stamp,
        }),
        services: checks
            .iter()
            .map(|c| {
                (
                    c.service.clone(),
                    ServiceRecord {
                        port: c.port.unwrap_or_default(),
                        pid: c.pid,
                        data_dir: c.data_dir.clone(),
                        identity: c.identity.unwrap_or(Identity::Liveness),
                        verified_at: c.checked_at,
                    },
                )
            })
            .collect(),
    };
    save(ctx, &session).map_err(|e| steps.clone().fail("record_session", e, true))?;
    Ok(UpReport {
        session,
        checks,
        steps: steps.0,
        reaped,
    })
}

#[derive(Debug, Serialize)]
pub struct DownReport {
    pub stopped: Vec<Value>,
    pub confirmed: bool,
}

/// Stop the project's services. Succeeds only once their processes are gone and ports closed.
pub fn down(ctx: &Ctx) -> Result<DownReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    down_locked(ctx)
}

fn down_locked(ctx: &Ctx) -> Result<DownReport> {
    // Failure to discover ownership cannot establish that nothing is running.
    let before = mise::daemons(&ctx.root)?;
    let session = load(ctx)?;
    let mut ports: Vec<u16> = ports::lookup(&ctx.state, &ctx.root)?
        .values()
        .copied()
        .collect();
    let mut pids: Vec<(String, u32)> = before
        .iter()
        .filter_map(|d| d.pid.map(|p| (d.name.clone(), p)))
        .collect();
    ports.extend(before.iter().filter_map(|d| d.port));
    if let Some(session) = &session {
        ports.extend(session.services.values().map(|s| s.port));
        pids.extend(
            session
                .services
                .iter()
                .filter_map(|(name, s)| s.pid.map(|p| (name.clone(), p))),
        );
    }
    ports.sort_unstable();
    ports.dedup();
    ports.retain(|p| *p != 0);
    pids.sort_unstable();
    pids.dedup();

    // Providers may list configured but never-started daemons. Stopping those returns
    // "no matching daemons"; only invoke stop when there is actual ownership to reconcile.
    let needs_stop = pids.iter().any(|(_, pid)| pid_alive(*pid))
        || before
            .iter()
            .any(|d| matches!(d.status.as_str(), "running" | "starting"))
        || ports.iter().any(|port| accepting(*port));
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
    Ok(DownReport {
        stopped: pids
            .iter()
            .map(|(name, pid)| json!({ "service": name, "pid": pid }))
            .collect(),
        confirmed: true,
    })
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
}

/// Always answers, even for a broken session: diagnosing that state is the point.
pub fn status(ctx: &Ctx) -> Result<StatusReport> {
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let session = load(ctx)?;
    let report = ctx.compile(false)?;
    let checks = if report.stack.services.is_empty() {
        Vec::new()
    } else {
        let env = mise::env(&ctx.root)?;
        let statuses = mise::daemons(&ctx.root)?;
        verify_session(ctx, &report, &env, &statuses, session.as_ref())
    };
    Ok(StatusReport {
        lease_expired: session
            .as_ref()
            .and_then(|s| s.lease.as_ref())
            .and_then(|l| l.expired(now())),
        stale: session
            .as_ref()
            .is_some_and(|s| s.config_digest != config_digest(ctx, &report)),
        session,
        checks,
    })
}

pub struct ExecPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Complete environment for the command, including poisoned endpoints.
    pub env: IndexMap<String, String>,
    /// Variables that must not be inherited from the caller either.
    pub removed: Vec<String>,
    pub checks: Vec<Check>,
}

/// Host that can never resolve (RFC 2606), so a poisoned endpoint fails loudly and says why.
pub const UNVERIFIED_HOST: &str = "unverified.stack.invalid";

/// Replace the host in an endpoint value. `None` if the value has no host to replace,
/// in which case the variable is removed instead.
fn poison(value: &str) -> Option<String> {
    let poisoned = value
        .replace("127.0.0.1", UNVERIFIED_HOST)
        .replace("localhost", UNVERIFIED_HOST);
    (poisoned != value).then_some(poisoned)
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
    let _guard = project_lock(&ctx.state, &ctx.root)?;
    let mut session = load(ctx)?;
    let report = ctx.compile(true)?;
    mise::trust(&ctx.root)?;
    let mut env = mise::env(&ctx.root)?;
    let checks = if report.stack.services.is_empty() {
        Vec::new()
    } else {
        verify_session(
            ctx,
            &report,
            &env,
            &mise::daemons(&ctx.root)?,
            session.as_ref(),
        )
    };

    let required: Vec<&String> = match require {
        Require::Nothing => Vec::new(),
        Require::All => report.stack.services.keys().collect(),
        Require::Only(names) => names.iter().collect(),
    };
    for name in &required {
        if !report.stack.services.contains_key(*name) {
            return Err(StackError::new(
                "unknown_service",
                format!("no service named '{name}'"),
            ));
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
    let mut removed = Vec::new();
    for var in checks.iter().flat_map(|c| c.withheld.iter()) {
        match env.get(var).and_then(|v| poison(v)) {
            Some(bad) => {
                env.insert(var.clone(), bad);
            }
            None => {
                env.shift_remove(var);
                removed.push(var.clone());
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

    if let Some(session) = session.as_mut().filter(|s| s.config_digest == config_digest(ctx, &report)) {
        if let Some(lease) = session.lease.as_mut() {
            lease.renewed_at = now();
            save(ctx, session)?;
        }
        env.insert("STACK_SESSION".into(), session.id.clone());
    } else {
        env.shift_remove("STACK_SESSION");
        removed.push("STACK_SESSION".into());
    }
    env.insert("STACK_PROJECT".into(), ctx.root.to_string_lossy().into());

    Ok(ExecPlan {
        program,
        args: args.to_vec(),
        env,
        removed,
        checks,
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
        if !session.project.exists() {
            out.push(GcEntry {
                project: session.project,
                reason: "project directory deleted".into(),
                stopped: false,
                error: Some(
                    "cannot stop services without the project; see `mise daemons prune`".into(),
                ),
            });
            continue;
        }
        let Some(reason) = session.lease.as_ref().and_then(|l| l.expired(now())) else {
            continue;
        };
        let result = down_locked(&ctx);
        out.push(GcEntry {
            project: session.project,
            reason,
            stopped: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
        });
    }
    Ok(out)
}

fn load(ctx: &Ctx) -> Result<Option<Session>> {
    // The machine index is authoritative. A crash between the two atomic writes can
    // leave the project copy behind; lifecycle operations always use the indexed generation.
    let path = if ctx.index_file().exists() {
        ctx.index_file()
    } else {
        ctx.session_file()
    };
    if !path.exists() {
        return Ok(None);
    }
    read_json::<Session>(&path).map(Some)
}

fn save(ctx: &Ctx, session: &Session) -> Result<()> {
    write_json(&ctx.index_file(), session)?;
    write_json(&ctx.session_file(), session)
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(crate::error::io_error(path.display(), e)),
    }
}

fn config_digest(ctx: &Ctx, report: &Report) -> String {
    let config =
        json!({ "lock": lock_digest(&ctx.root), "stack": report.stack, "ports": report.ports });
    sha256_hex(config.to_string().as_bytes())
}

fn verify_session(
    ctx: &Ctx,
    report: &Report,
    env: &IndexMap<String, String>,
    statuses: &[DaemonStatus],
    session: Option<&Session>,
) -> Vec<Check> {
    let mut checks = verify_all(report, env, statuses);
    let reason = match session {
        None => Some("no launch record; run `stack up`"),
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
            );
        }
    }
    checks
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
    fn poison_replaces_hosts_or_declines() {
        assert_eq!(
            poison("postgresql://postgres@127.0.0.1:41234/postgres").unwrap(),
            "postgresql://postgres@unverified.stack.invalid:41234/postgres"
        );
        assert_eq!(poison("127.0.0.1").unwrap(), UNVERIFIED_HOST);
        assert_eq!(
            poison("redis://localhost:6379").unwrap(),
            "redis://unverified.stack.invalid:6379"
        );
        assert_eq!(poison("41234"), None);
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
        let env: IndexMap<String, String> = [
            (
                "DATABASE_URL",
                "postgresql://postgres@127.0.0.1:41234/postgres",
            ),
            ("PGPORT", "41234"),
            ("PGHOST", "127.0.0.1"),
            ("APP_DB", "host=127.0.0.1:41234"),
            ("REDIS_URL", "redis://127.0.0.1:45555"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let pg = Service {
            preset: Some("postgres".into()),
            version: None,
            run: None,
            ready_cmd: None,
            ready_port: None,
            port: None,
        };
        let vars = binding_vars("postgres", &pg, Some(41234), &env);
        for v in ["DATABASE_URL", "PGPORT", "PGHOST", "APP_DB"] {
            assert!(vars.contains(&v.to_string()), "{v} not withheld: {vars:?}");
        }
        assert!(!vars.contains(&"REDIS_URL".to_string()));
        assert!(!vars.contains(&"PATH".to_string()));
    }
}
