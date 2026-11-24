//! Stack's own service supervisor: one small process per checkout that runs the checkout's
//! services and nothing else.
//!
//! It is the `stack` binary re-executed as `stack __supervise <dir>`, detached in a session of
//! its own, so it outlives the command that started it and no terminal signal reaches it.
//! Everything it knows is in `<dir>` (under Stack's state directory, keyed by the checkout's
//! path, see [`dir`]):
//!
//! - `spec.json`, what should run: each service's command, environment, port, readiness check
//!   and stop signal, whether it is wanted, and whether the supervisor should stop everything and
//!   exit. Written only by Stack commands holding the checkout's project lock, atomically, with a
//!   revision that increases on every write.
//! - `state.json`, what runs: the supervisor's own pid and process start time, the revision it
//!   last applied, and for each service its status, pid, process start time, process group,
//!   port and data directory. Written only by the supervisor (or, once it is gone, by the command
//!   that cleaned up after it), atomically.
//! - `lock`, held exclusively by the live supervisor, so two never run for one checkout.
//! - `logs/<service>.log`, each line of the service's output prefixed with when it was read
//!   (Unix milliseconds), which `stack logs` reads.
//!
//! A command changes `spec.json` and sends the supervisor `SIGHUP` (bound to the pid and start
//! time `state.json` records), or starts one when none runs, then waits until `state.json` says
//! the revision was applied. Nothing is shared between checkouts: no machine-wide daemon, no
//! socket, no lock another worktree takes.
//!
//! Each service runs in a session and process group of its own, with its output on a pipe the
//! supervisor drains. Stopping signals the whole group (`SIGINT`, Postgres's fast shutdown, for
//! Postgres; `SIGTERM` otherwise), then `SIGKILL`s the group after [`STOP_GRACE`]. Listing what
//! runs is a read of `state.json` and a pid start-time check: no process is started for it.
//!
//! When the supervisor itself is killed, its services keep running (on Linux the main process of
//! each gets its stop signal from the kernel, `PR_SET_PDEATHSIG`); the next command finds them
//! in `state.json` by pid, start time and group, and stops them before anything else.

use crate::error::{io_error, Result, StackError};
use crate::procinfo;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The hidden argument that runs the supervisor: `stack __supervise <dir>`.
pub const SUPERVISE: &str = "__supervise";
/// The hidden argument that initialises a preset's data and runs its server:
/// `stack __preset <preset> <data dir> <port> [<pinned version>] [<role>=<port>...] [templates=<dir>]`.
pub const PRESET: &str = "__preset";

/// How long a service may take to stop after its stop signal before its group is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long a command waits for the supervisor to apply a change.
const APPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a command waits for stopped services to be gone: past [`STOP_GRACE`], the kill.
const STOP_WAIT: Duration = Duration::from_secs(15);
/// A `ready_cmd` (or a preset's readiness command) run longer than this has failed once.
const READY_CMD_TIMEOUT: Duration = Duration::from_secs(5);
/// Log files are rotated (one older file kept) once they pass this size.
const LOG_ROTATE_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes of log `stack logs` reads at most.
pub const LOGS_LIMIT: u64 = 1024 * 1024;
/// The file the data directory's preset and major version are recorded in. mise's presets
/// wrote the same file, so data they initialised is recognised, and theirs ours.
pub const VERSION_FILE: &str = ".mise-daemon-version";

/// The supervisor directory of the checkout at `root` in the Stack state `state`.
pub fn dir(state: &Path, root: &Path) -> PathBuf {
    state.join("supervisors").join(crate::state::project_key(root))
}

/// What is a command, never to be signalled, for the checkout whose supervisor directory (see
/// [`dir`]) this is: its Stack state's registries and the checkout the spec names.
fn exempt_for(dir: &Path) -> crate::commands::Exempt {
    let state = dir.parent().and_then(Path::parent).unwrap_or(dir);
    let project = read_spec(dir).ok().flatten().and_then(|s| s.project);
    crate::commands::Exempt::load(state, project.as_deref())
}

fn spec_path(dir: &Path) -> PathBuf {
    dir.join("spec.json")
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

/// Where the supervisor keeps `service`'s output.
pub fn log_path(dir: &Path, service: &str) -> PathBuf {
    dir.join("logs").join(format!("{service}.log"))
}

// ---- records -------------------------------------------------------------------------------

/// What should run. Written by commands under the project lock.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// Increases on every write; the supervisor reports the last it applied.
    pub revision: u64,
    /// Stop every service and exit.
    #[serde(default)]
    pub shutdown: bool,
    /// The checkout the services are of, so `gc` can tell when it is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// The checkout's token, which every process of its services carries: how what a dead
    /// supervisor left is told from other processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub services: IndexMap<String, ServiceSpec>,
}

/// How to run one service, and whether it should run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// Program and arguments, run directly (a `run` command is `sh -c <run>`).
    pub argv: Vec<String>,
    /// The whole environment: nothing else is inherited.
    pub env: IndexMap<String, String>,
    pub cwd: PathBuf,
    pub port: u16,
    pub ready: Ready,
    /// Sent to the service's process group to stop it.
    pub stop_signal: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// Times a service that exits on its own is started again (with a growing delay) before it
    /// is left `failed`. Zero, as no manifest field sets it yet: a service that exits stays down.
    #[serde(default)]
    pub retries: u32,
    pub want: bool,
    /// Raised by a command each time it asks for the service to run: one that ended on its own
    /// is started again only when this rose since it was started (or on a retry).
    #[serde(default)]
    pub start_seq: u64,
}

/// When a started service counts as running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ready {
    /// As soon as it was spawned.
    Spawned,
    /// Once a TCP connection to 127.0.0.1 on this port succeeds.
    Port { port: u16 },
    /// Once this command, run through `sh -c` with the service's environment, exits 0.
    Command { command: String },
}

/// What runs. Written by the supervisor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The supervisor writing this, while it runs; `None` once it exited.
    #[serde(default)]
    pub supervisor: Option<Proc>,
    /// The last revision of `spec.json` it acted on.
    #[serde(default)]
    pub applied: u64,
    #[serde(default)]
    pub services: IndexMap<String, Instance>,
    /// The checkout this record is of, as the spec it acted on named it (path and token), so a
    /// read of this one file says whose processes it names (see [`shutdown_accounted`]).
    /// Absent in records Stack 0.1.23's supervisor wrote, which are then no evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// A process by pid, start time (see [`procinfo::Process::started`]) and boot (see
/// [`procinfo::boot_id`]), so neither a reused pid nor a record from before a reboot is taken
/// for it. A record without a boot names nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proc {
    pub pid: u32,
    pub started: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
}

impl Proc {
    /// The process `pid` now, as a record.
    pub fn observe(pid: u32) -> Option<Proc> {
        let p = procinfo::process(pid)?;
        Some(Proc { pid, started: p.started, boot: procinfo::boot_id() })
    }

    /// Recorded in this boot of the machine.
    pub fn this_boot(&self) -> bool {
        self.boot.is_some() && self.boot == procinfo::boot_id()
    }

    /// Still running: recorded in this boot, and the pid names a process with the recorded
    /// start time.
    pub fn alive(&self) -> bool {
        self.current().is_some()
    }

    /// The process the record names, observed now, when it still runs.
    pub fn current(&self) -> Option<procinfo::Process> {
        if !self.this_boot() {
            return None;
        }
        procinfo::process(self.pid).filter(|p| p.started == self.started)
    }
}

/// One service as the supervisor last saw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    /// `starting`, `running`, `stopping`, `stopped` or `failed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Proc>,
    /// The service's process group (its pid: each service leads its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    pub stop_signal: i32,
    /// When it was about to be spawned, Unix milliseconds: every process of it started later.
    #[serde(default)]
    pub started_at_ms: u64,
    /// How its last process ended (`exit code 1`, `signal 9`), once it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<String>,
    #[serde(default)]
    pub restarts: u32,
}

impl Instance {
    /// The process, when it still runs.
    pub fn live(&self) -> Option<Proc> {
        self.process.clone().filter(Proc::alive)
    }
}

/// One supervised service as commands see it: what Stack's checks and records are made from.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DaemonStatus {
    /// The supervisor's id for the service, where it has one (Pitchfork's qualified id for a
    /// session an older Stack started; none for Stack's own supervisor).
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub status: String,
    /// Its process, only while one with the recorded start time runs (whatever `status` says).
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub data_dir: Option<String>,
}

fn invalid(path: &Path, e: impl std::fmt::Display) -> StackError {
    let hint = if path.file_name().is_some_and(|n| n == "spec.json") {
        "Stack cannot tell what this checkout's supervisor was asked to run. Deleting the file tells a supervisor still running to stop every service of the checkout and exit (then `stack down` confirms they are gone, and `stack up` starts them again)"
    } else {
        "Stack cannot tell what this checkout's supervisor runs. Check that nothing of the checkout still runs (`stack ps`), stop what does yourself, then delete the file"
    };
    StackError::new("state_invalid", format!("{}: {e}", path.display())).hint(hint)
}

fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| invalid(path, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

/// `spec.json`: `None` when there is none; an error when one exists that cannot be read.
pub fn read_spec(dir: &Path) -> Result<Option<Spec>> {
    read_record(&spec_path(dir))
}

/// `state.json`: `None` when there is none; an error when one exists that cannot be read.
pub fn read_state(dir: &Path) -> Result<Option<State>> {
    read_record(&state_path(dir))
}

/// `state.json` for listing only: one that cannot be read lists as nothing.
pub fn state_lossy(dir: &Path) -> State {
    read_state(dir).ok().flatten().unwrap_or_default()
}

/// Whether the supervisor `state` names still runs.
pub fn alive(state: &State) -> bool {
    state.supervisor.as_ref().is_some_and(Proc::alive)
}

/// Replace `path` with `bytes` in one rename. Not synced: a crash loses at most the last write
/// of a file that describes processes, which a crash ends anyway.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    stage(path, bytes)?.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A temporary file beside `path` holding all of `bytes`, readable by its owner only (the mode
/// set on the file itself, never on `path` after it is published), ready to rename over it.
fn stage(path: &Path, bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(tmp)
}

/// What the supervisor in `dir` reports for each service, with a status for every service of
/// `configured` (name and port) it has no record of (`available`). A process that is gone is
/// reported without its pid; one that runs while no supervisor does is `orphaned`. Only files
/// are read, and each pid's start time.
pub fn daemons(dir: &Path, configured: &[(String, u16, Option<PathBuf>)]) -> Vec<DaemonStatus> {
    let state = state_lossy(dir);
    let supervised = alive(&state);
    let mut out: Vec<DaemonStatus> = state
        .services
        .iter()
        .map(|(name, i)| {
            let live = i.live();
            let status = match (&live, i.status.as_str()) {
                (Some(_), _) if !supervised => "orphaned".to_string(),
                (Some(_), s) => s.to_string(),
                (None, "stopping" | "stopped") => "stopped".to_string(),
                (None, _) if !supervised && i.status != "failed" => "stopped".to_string(),
                (None, "starting" | "running") => "failed".to_string(),
                (None, s) => s.to_string(),
            };
            DaemonStatus {
                id: None,
                name: name.clone(),
                status,
                pid: live.map(|p| p.pid),
                port: Some(i.port),
                data_dir: i.data_dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
            }
        })
        .collect();
    for (name, port, data_dir) in configured {
        if !out.iter().any(|d| d.name == *name) {
            out.push(DaemonStatus {
                id: None,
                name: name.clone(),
                status: "available".into(),
                pid: None,
                port: Some(*port),
                data_dir: data_dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
            });
        } else if let Some(d) = out.iter_mut().find(|d| d.name == *name && d.data_dir.is_none()) {
            d.data_dir = data_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
        }
    }
    out
}

// ---- commands ------------------------------------------------------------------------------

fn failed(code: &'static str, message: String, dir: &Path) -> StackError {
    let log = fs::read_to_string(dir.join("supervisor.log")).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
    let mut e = StackError::new(code, message).hint("retry; `stack down` stops whatever the supervisor started");
    if !tail.is_empty() {
        e = e.with_detail(serde_json::json!({ "supervisor_log": tail.join("\n") }));
    }
    e
}

/// `stop_unconfirmed` for what recovery could not show stopped: nothing new is started while it
/// may run.
fn unconfirmed(left: Vec<serde_json::Value>) -> StackError {
    StackError::new("stop_unconfirmed", "processes a stopped supervisor left could not be confirmed stopped, so nothing new was started")
        .hint("Stack signals only processes it can show are this checkout's (its token, started after the service, in this boot); stop the listed pids yourself if they are, then retry")
        .details(left)
}

/// Send `signal` to the supervisor `state` names, when it still runs. Whether it was sent.
fn signal_supervisor(state: &State, signal: i32) -> bool {
    let Some(p) = state.supervisor.as_ref().filter(|p| p.alive()) else { return false };
    // SAFETY: kill only sends a signal, to the process just confirmed to be the supervisor (a
    // pid observed again right before; see `crate::stopping` on the gap this leaves).
    unsafe { libc::kill(p.pid as libc::pid_t, signal) == 0 }
}

/// Most bytes `supervisor.log` keeps; past this it is cut to its last [`SUPERVISOR_LOG_KEEP`].
const SUPERVISOR_LOG_MAX: u64 = 1024 * 1024;
const SUPERVISOR_LOG_KEEP: u64 = 256 * 1024;

/// Cut `path` to its last [`SUPERVISOR_LOG_KEEP`] bytes, from a line boundary, once it is past
/// [`SUPERVISOR_LOG_MAX`].
fn cap_log(path: &Path) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= SUPERVISOR_LOG_MAX {
        return;
    }
    let Ok(mut f) = fs::File::open(path) else { return };
    let mut tail = Vec::new();
    if f.seek(std::io::SeekFrom::Start(meta.len() - SUPERVISOR_LOG_KEEP)).is_err() || f.read_to_end(&mut tail).is_err() {
        return;
    }
    let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    let _ = write_atomic(path, &tail[start..]);
}

/// [`cap_log`] for the running supervisor's own `supervisor.log`, which is its stderr (opened
/// for appending, so what it writes after the cut goes after the kept tail): checked every pass,
/// a `fstat` only, so a supervisor that runs for long cannot grow it without bound.
fn cap_own_log(dir: &Path) {
    use std::os::unix::fs::MetadataExt;
    let path = dir.join("supervisor.log");
    // SAFETY: fstat writes into the zeroed struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(2, &mut st) } != 0 || st.st_size as u64 <= SUPERVISOR_LOG_MAX {
        return;
    }
    // Only when stderr is that file (not when started some other way).
    let Ok(meta) = fs::metadata(&path) else { return };
    #[allow(clippy::unnecessary_cast)]
    if meta.ino() != st.st_ino as u64 || meta.dev() != st.st_dev as u64 {
        return;
    }
    let Ok(mut f) = fs::File::open(&path) else { return };
    let mut tail = Vec::new();
    if f.seek(std::io::SeekFrom::Start((st.st_size as u64).saturating_sub(SUPERVISOR_LOG_KEEP))).is_err() || f.read_to_end(&mut tail).is_err() {
        return;
    }
    let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    let tail = &tail[start..];
    // SAFETY: ftruncate and write on this process's own stderr, with a buffer it owns.
    unsafe {
        if libc::ftruncate(2, 0) == 0 {
            let _ = libc::write(2, tail.as_ptr().cast(), tail.len());
        }
    }
}

/// Start a supervisor for `dir`, detached in a session of its own. Returns once it is forked
/// (the process started here forks it and exits at once, so nothing is left to reap).
fn spawn(dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| StackError::new("start_failed", format!("cannot find the stack executable: {e}")))?;
    fs::create_dir_all(dir.join("logs")).map_err(|e| io_error(dir.display(), e))?;
    let log_path = dir.join("supervisor.log");
    cap_log(&log_path);
    let log = fs::OpenOptions::new().create(true).append(true).open(&log_path).map_err(|e| io_error(dir.display(), e))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(SUPERVISE)
        .arg(dir)
        .env_clear()
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| io_error(dir.display(), e))?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command
        .status()
        .map_err(|e| StackError::new("start_failed", format!("cannot start the service supervisor: {e}")))?;
    if !status.success() {
        return Err(failed("start_failed", format!("the service supervisor did not start ({status})"), dir));
    }
    Ok(())
}

/// Write `spec.json` with `change` applied, have the supervisor apply it (starting one when none
/// runs, after stopping what a dead one left running), and return its state once it did.
///
/// The revision written is above both the spec's and the last one the state acknowledges, so
/// an acknowledgment can only come from a supervisor that read this request. A spec or state
/// that exists but cannot be read is an error, never a fresh start.
pub fn apply(dir: &Path, change: impl FnOnce(&mut Spec)) -> Result<State> {
    let mut spec = read_spec(dir)?.unwrap_or_default();
    let state = read_state(dir)?.unwrap_or_default();
    change(&mut spec);
    spec.revision = spec.revision.max(state.applied) + 1;
    let bytes = serde_json::to_vec_pretty(&spec).expect("spec serializes");
    write_atomic(&spec_path(dir), &bytes).map_err(|e| io_error(spec_path(dir).display(), e))?;
    let mut spawned_at = None;
    if !signal_supervisor(&state, libc::SIGHUP) {
        let left = recover(dir, &state, spec.token.as_deref());
        if !left.is_empty() {
            return Err(unconfirmed(left));
        }
        if spec.shutdown {
            // Nothing to start: record that the dead supervisor's services are gone.
            let done = State { supervisor: None, applied: spec.revision, services: settled(&state.services), project: spec.project.clone(), token: spec.token.clone() };
            write_state(dir, &done).map_err(|e| io_error(state_path(dir).display(), e))?;
            return Ok(done);
        }
        spawn(dir)?;
        spawned_at = Some(Instant::now());
    }
    let deadline = Instant::now() + crate::process::bounded(APPLY_TIMEOUT);
    let mut respawned = false;
    let mut pause = Duration::from_millis(1);
    loop {
        let now = state_lossy(dir);
        if now.applied >= spec.revision && (alive(&now) || spec.shutdown) {
            return Ok(now);
        }
        // A supervisor exiting as the change was written never sees it: start another once.
        if !alive(&now) && !respawned && spawned_at.map_or(true, |at| at.elapsed() > Duration::from_millis(500)) {
            if now.applied >= spec.revision {
                return Ok(now);
            }
            let left = recover(dir, &now, spec.token.as_deref());
            if !left.is_empty() {
                return Err(unconfirmed(left));
            }
            if spec.shutdown {
                let done = State { supervisor: None, applied: spec.revision, services: settled(&now.services), project: spec.project.clone(), token: spec.token.clone() };
                write_state(dir, &done).map_err(|e| io_error(state_path(dir).display(), e))?;
                return Ok(done);
            }
            respawned = true;
            spawn(dir)?;
            spawned_at = Some(Instant::now());
        }
        if crate::process::expired() {
            return Err(StackError::new("timed_out", "the deadline passed while the service supervisor applied a change"));
        }
        if Instant::now() >= deadline {
            return Err(failed("start_failed", format!("the service supervisor in {} did not answer within {}s", dir.display(), APPLY_TIMEOUT.as_secs()), dir));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(10));
    }
}

/// Every instance recorded as stopped (what remains after everything was stopped).
fn settled(services: &IndexMap<String, Instance>) -> IndexMap<String, Instance> {
    services
        .iter()
        .map(|(n, i)| {
            let mut i = i.clone();
            if i.live().is_none() {
                i.process = None;
                i.group = None;
                if i.status != "failed" {
                    i.status = "stopped".into();
                }
            }
            (n.clone(), i)
        })
        .collect()
}

/// Never creates `dir`: a supervisor whose directory was removed must not bring it back (with
/// every parent the removal took) by writing its last record.
fn write_state(dir: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).expect("state serializes");
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.persist(state_path(dir)).map_err(|e| e.error)?;
    Ok(())
}


/// Make `specs` the services' definitions and run `names` (others keep running, or not, as they
/// were). Returns once the supervisor spawned them.
pub fn start(dir: &Path, project: &Path, specs: &IndexMap<String, ServiceSpec>, names: &[String]) -> Result<State> {
    apply(dir, |spec| {
        spec.shutdown = false;
        spec.project = Some(project.to_path_buf());
        spec.token = specs.values().find_map(|s| s.env.get(crate::checkout::TOKEN_VAR).cloned()).or(spec.token.take());
        let previous = std::mem::take(&mut spec.services);
        for (name, def) in specs {
            let mut def = def.clone();
            let before = previous.get(name);
            def.want = names.contains(name) || before.is_some_and(|p| p.want);
            def.start_seq = before.map_or(0, |p| p.start_seq) + u64::from(names.contains(name));
            spec.services.insert(name.clone(), def);
        }
    })
}

/// Stop `names` and wait until their processes are gone (`stop_unconfirmed` otherwise).
pub fn stop(dir: &Path, names: &[String]) -> Result<()> {
    if read_spec(dir)?.is_none() && read_state(dir)?.is_none() {
        return Ok(());
    }
    apply(dir, |spec| {
        for name in names {
            if let Some(s) = spec.services.get_mut(name) {
                s.want = false;
            }
        }
    })?;
    wait_gone(dir, Some(names))
}

/// The services `spec` ever asked to run (wanted now, or started at some point): what a
/// `state.json` must give an account of (see [`accounts_for`]).
pub fn asked_to_run(spec: &Spec) -> Vec<String> {
    spec.services.iter().filter(|(_, s)| s.want || s.start_seq > 0).map(|(n, _)| n.clone()).collect()
}

/// Whether `state` is affirmative evidence of what became of every service in `asked` (see
/// [`asked_to_run`]) under `spec`: it acknowledged a request (`applied` at least 1) and none
/// the spec never made (`applied` at most its revision), it records each of those services,
/// and every process it names (the supervisor's and each service's) carries the boot it ran
/// in. A `state.json` that only parses (`{}`, every field defaulted) is not evidence. Why not,
/// when it is not.
pub fn accounts_for(spec: &Spec, state: &State, asked: &[String]) -> std::result::Result<(), String> {
    if state.applied == 0 {
        return Err("its state.json records no request the supervisor acted on".into());
    }
    if state.applied > spec.revision {
        return Err(format!("its state.json acknowledges request {} of a spec that made {}", state.applied, spec.revision));
    }
    let missing: Vec<&str> = asked.iter().filter(|n| !state.services.contains_key(*n)).map(String::as_str).collect();
    if !missing.is_empty() {
        return Err(format!("its state.json records nothing of {}", missing.join(", ")));
    }
    let processes = state.supervisor.iter().chain(state.services.values().filter_map(|i| i.process.as_ref()));
    if processes.into_iter().any(|p| p.boot.is_none()) {
        return Err("its state.json names a process without the boot it ran in".into());
    }
    Ok(())
}

/// The checkout a recovering `down` expects a supervisor record to be of, and the services that
/// record must account for (see [`accounts_for`]).
#[derive(Debug, Clone)]
pub struct Expected<'a> {
    pub project: &'a Path,
    pub token: &'a str,
    pub asked: &'a [String],
}

/// [`shutdown`] for a checkout whose own session record is lost (`stack down` recovering it):
/// only on the evidence of [`accounts_for`] in a record of exactly the `expected` checkout (its
/// `spec.json` naming that path and token), before the shutdown is written, when it is written,
/// and on every look while waiting; strays are looked for by the expected token, never by one
/// read back. A `spec.json` or `state.json` that goes missing, cannot be read, names another
/// checkout or stops being that evidence is `state_invalid`: an empty list of live processes
/// then proves nothing.
pub fn shutdown_accounted(dir: &Path, expected: &Expected) -> Result<()> {
    // Neither a check and the write after it nor a check and the signal after it can be made
    // atomic against a program that replaces files in Stack's own state directory. What this
    // does instead: every state it acts on identifies itself (its own `project` and `token`)
    // in the same read that is acted on; every write is staged completely, mode included, in a
    // temporary file, and only then is the file it replaces read again (the spec must still be
    // the very file checked, the state must name this checkout), with nothing but the rename
    // after that check; nothing is chmod'ed by name after publishing; and nothing is ever
    // signalled or written on the strength of a state that does not name this checkout. The
    // window left is exactly the instant between that check and the rename.
    seam("evidence");
    evidence(dir, expected)?;
    let (mut spec, checked) = read_spec_identified(dir)?;
    ours(dir, &spec, expected).map_err(|why| not_evidence(dir, why))?;
    seam("after_spec_check");
    // The state the shutdown and the signal are decided from, read once and checked.
    let state = identified_state(dir, expected)?;
    accounts_for(&spec, &state, expected.asked).map_err(|why| not_evidence(dir, why))?;
    spec.shutdown = true;
    spec.services.values_mut().for_each(|s| s.want = false);
    spec.revision = spec.revision.max(state.applied) + 1;
    // Staged completely first (mode included); then the spec about to be replaced is read
    // again, and must be the same file and still this checkout's; then only the rename.
    let path = spec_path(dir);
    let bytes = serde_json::to_vec_pretty(&spec).expect("spec serializes");
    let staged = stage(&path, &bytes).map_err(|e| io_error(path.display(), e))?;
    seam("spec_staged");
    let (again, now) = read_spec_identified(dir)?;
    if now != checked {
        return Err(not_evidence(dir, "its spec.json was replaced".into()));
    }
    ours(dir, &again, expected).map_err(|why| not_evidence(dir, why))?;
    staged.persist(&path).map_err(|e| io_error(path.display(), e.error))?;
    // The pid signalled is the one the checked state names.
    if !signal_supervisor(&state, libc::SIGHUP) {
        settle_dead(dir, expected, &state, spec.revision)?;
    } else {
        let deadline = Instant::now() + crate::process::bounded(APPLY_TIMEOUT);
        let mut pause = Duration::from_millis(1);
        loop {
            seam("ack_poll");
            let now = identified_state(dir, expected)?;
            if now.applied >= spec.revision {
                break;
            }
            if !alive(&now) {
                settle_dead(dir, expected, &now, spec.revision)?;
                break;
            }
            if crate::process::expired() {
                return Err(StackError::new("timed_out", "the deadline passed while the service supervisor applied a change"));
            }
            if Instant::now() >= deadline {
                return Err(failed("stop_unconfirmed", format!("the service supervisor in {} did not answer within {}s", dir.display(), APPLY_TIMEOUT.as_secs()), dir));
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(10));
        }
    }
    wait_gone_with(dir, None, Some(expected))
}

/// For a recovering shutdown whose supervisor is dead (`state`, read and identified): stop what
/// it left by the expected token, then record its services gone at `revision`, after reading
/// the state about to be replaced again and finding it still this checkout's.
fn settle_dead(dir: &Path, expected: &Expected, state: &State, revision: u64) -> Result<()> {
    let left = recover(dir, state, Some(expected.token));
    if !left.is_empty() {
        return Err(unconfirmed(left));
    }
    let done = State { supervisor: None, applied: revision, services: settled(&state.services), ..state.clone() };
    publish_state_checked(dir, expected, &done)
}

/// Write `state` in a recovering shutdown: staged completely (mode included), then the state
/// about to be replaced read again and required to name this checkout, then only the rename.
fn publish_state_checked(dir: &Path, expected: &Expected, state: &State) -> Result<()> {
    let path = state_path(dir);
    let bytes = serde_json::to_vec_pretty(state).expect("state serializes");
    let staged = stage(&path, &bytes).map_err(|e| io_error(path.display(), e))?;
    seam("state_staged");
    identified_state(dir, expected)?;
    staged.persist(&path).map_err(|e| io_error(path.display(), e.error))?;
    Ok(())
}

/// `spec.json` and the device and inode of the file it was read from.
fn read_spec_identified(dir: &Path) -> Result<(Spec, (u64, u64))> {
    use std::os::unix::fs::MetadataExt;
    let path = spec_path(dir);
    let gone = || not_evidence(dir, "its spec.json is gone".into());
    let mut file = fs::File::open(&path).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { gone() } else { io_error(path.display(), e) })?;
    let meta = file.metadata().map_err(|e| io_error(path.display(), e))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|e| io_error(path.display(), e))?;
    let spec = serde_json::from_slice(&bytes).map_err(|e| invalid(&path, e))?;
    Ok((spec, (meta.dev(), meta.ino())))
}

/// `state.json`, only when it names the `expected` checkout itself.
fn identified_state(dir: &Path, expected: &Expected) -> Result<State> {
    let state = read_state(dir)?.ok_or_else(|| not_evidence(dir, "its state.json is gone".into()))?;
    if state.project.as_deref() != Some(expected.project) || state.token.as_deref() != Some(expected.token) {
        return Err(not_evidence(dir, format!("its state.json in {} does not name this checkout", dir.display())));
    }
    Ok(state)
}

/// What a recovering shutdown checks at each named point; a test can act there.
fn seam(_at: &'static str) {
    #[cfg(test)]
    tests::at_evidence_seam(_at);
}

fn not_evidence(dir: &Path, why: String) -> StackError {
    StackError::new("state_invalid", format!("the supervisor record in {} is not evidence of what runs: {why}", dir.display()))
        .hint("nothing was stopped on its word and every record was kept: check what may still run for this checkout and stop it yourself")
}

/// Whether `spec` is of the `expected` checkout: its path and its token.
fn ours(dir: &Path, spec: &Spec, expected: &Expected) -> std::result::Result<(), String> {
    if spec.project.as_deref() != Some(expected.project) || spec.token.as_deref() != Some(expected.token) {
        return Err(format!("its spec.json in {} is not this checkout's", dir.display()));
    }
    Ok(())
}

/// The state `dir` records, when it is [`accounts_for`] evidence in a record of the `expected`
/// checkout; `state_invalid` otherwise.
fn evidence(dir: &Path, expected: &Expected) -> Result<State> {
    let lost = |why: String| not_evidence(dir, why);
    let spec = read_spec(dir)?.ok_or_else(|| lost("its spec.json is gone".into()))?;
    ours(dir, &spec, expected).map_err(lost)?;
    let state = read_state(dir)?.ok_or_else(|| lost("its state.json is gone".into()))?;
    accounts_for(&spec, &state, expected.asked).map_err(lost)?;
    if state.project.as_deref() != Some(expected.project) || state.token.as_deref() != Some(expected.token) {
        return Err(lost(format!("its state.json in {} does not name this checkout", dir.display())));
    }
    Ok(state)
}

/// Stop every service and the supervisor, and wait until all of them are gone.
pub fn shutdown(dir: &Path) -> Result<()> {
    if read_spec(dir)?.is_none() && read_state(dir)?.is_none() {
        return Ok(());
    }
    apply(dir, |spec| {
        spec.shutdown = true;
        spec.services.values_mut().for_each(|s| s.want = false);
    })?;
    wait_gone(dir, None)
}

/// Wait until the named services' processes (every one's, and the supervisor, with `None`)
/// are gone. What a supervisor that died meanwhile left is stopped here.
fn wait_gone(dir: &Path, names: Option<&[String]>) -> Result<()> {
    wait_gone_with(dir, names, None)
}

/// [`wait_gone`]; with `accounted`, each look at the state must be evidence of the expected
/// checkout (see [`shutdown_accounted`]).
fn wait_gone_with(dir: &Path, names: Option<&[String]>, accounted: Option<&Expected>) -> Result<()> {
    let deadline = Instant::now() + crate::process::bounded(STOP_WAIT);
    let mut pause = Duration::from_millis(2);
    loop {
        if accounted.is_some() {
            seam("wait_poll");
        }
        let state = match accounted {
            Some(expected) => evidence(dir, expected)?,
            None => state_lossy(dir),
        };
        let left: Vec<serde_json::Value> = state
            .services
            .iter()
            .filter(|(n, _)| names.map_or(true, |names| names.contains(n)))
            .filter_map(|(n, i)| i.live().map(|p| serde_json::json!({ "service": n, "pid": p.pid })))
            .collect();
        let supervisor = names.is_none() && alive(&state);
        if left.is_empty() && !supervisor {
            return Ok(());
        }
        if !alive(&state) {
            // Recovering: the expected token, never what the spec says now.
            let token = match accounted {
                Some(expected) => Some(expected.token.to_string()),
                None => read_spec(dir).ok().flatten().and_then(|s| s.token),
            };
            let survivors = recover(dir, &state, token.as_deref());
            if !survivors.is_empty() {
                return Err(unconfirmed(survivors));
            }
            if names.is_none() {
                let settled_state = State { supervisor: None, applied: state.applied, services: settled(&state.services), ..state.clone() };
                match accounted {
                    Some(expected) => publish_state_checked(dir, expected, &settled_state)?,
                    None => write_state(dir, &settled_state).map_err(|e| io_error(state_path(dir).display(), e))?,
                }
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            let mut details = left;
            if supervisor {
                details.push(serde_json::json!({ "supervisor": state.supervisor.as_ref().map(|p| p.pid) }));
            }
            return Err(StackError::new("stop_unconfirmed", "services did not stop")
                .hint("retry `stack down`; stopping is safe to repeat")
                .details(details));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// Remove the supervisor directory `dir` (its spec, state and the services' logs) once nothing
/// it recorded runs. Whether it was removed (or was not there).
pub fn forget(dir: &Path) -> bool {
    let state = state_lossy(dir);
    if alive(&state) || !unresolved(dir).is_empty() {
        return false;
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Supervisor directories in the Stack state `state` whose checkout no longer exists, with that
/// checkout's path.
pub fn orphaned_dirs(state: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = fs::read_dir(state.join("supervisors")) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let project = read_spec(&e.path()).ok()??.project?;
            (!project.exists()).then(|| (e.path(), project))
        })
        .collect()
}

// ---- recovery ------------------------------------------------------------------------------

/// How long before a service's recorded start one of its processes may appear to have started:
/// Linux reports start times from a boot time in whole seconds.
const START_SLACK_MS: u64 = 2000;

/// Whether `p`, observed, is a process of this checkout started for a service recorded as
/// started at `started_at_ms`: it carries the checkout's token (read between two observations
/// of it) and started no earlier than that, and is no command Stack started (a service may run
/// `stack exec`, whose processes inherit the token; see [`crate::commands::Exempt`]).
fn owned(p: &procinfo::Process, token: Option<&str>, started_at_ms: u64, exempt: &crate::commands::Exempt) -> bool {
    let Some(token) = token else { return false };
    procinfo::environ_has(p.pid, crate::checkout::TOKEN_VAR, token) == Some(true)
        && !exempt.covers(p)
        && p.current().is_some()
        && p.started_unix_ms().is_some_and(|ms| ms + START_SLACK_MS >= started_at_ms)
}

/// What a supervisor that is no longer running left, from its `state`: each service whose
/// recorded leader still runs (by pid, start time and boot), to be stopped with the group it
/// still leads; every process of a recorded group whose leader is gone, and every process
/// carrying the checkout's token outside those groups, that is shown to be the checkout's
/// (see [`owned`]); and, not to be signalled, what is in a recorded group but cannot be shown
/// to be the checkout's.
///
/// A record from another boot names nothing. A leader pid in use by a process with another
/// start time means the recorded group has no member left (a pid is not handed out while a
/// group of that number has members). A descendant that left its group and dropped the token
/// is not found.
fn scan(state: &State, token: Option<&str>, exempt: &crate::commands::Exempt) -> (Vec<crate::stopping::Stray>, Vec<procinfo::Process>, Vec<serde_json::Value>) {
    let (mut strays, mut others, mut unverified) = (Vec::new(), Vec::<procinfo::Process>::new(), Vec::new());
    let mut earliest: Option<u64> = None;
    let mut any = false;
    for (name, i) in &state.services {
        let Some(record) = i.process.as_ref().filter(|r| r.this_boot()) else { continue };
        any = true;
        earliest = Some(earliest.map_or(i.started_at_ms, |e| e.min(i.started_at_ms)));
        let group = i.group.unwrap_or(record.pid);
        match procinfo::process(record.pid) {
            Some(leader) if leader.started == record.started => {
                // A group with a command in it (see `owned`) is not signalled as a group: its
                // leader is, and what stays is looked at member by member once it is gone.
                let leads = leader.group == group && group == leader.pid && !procinfo::in_group(group).iter().any(|m| exempt.covers(m));
                strays.push(crate::stopping::Stray {
                    service: name.clone(),
                    port: None,
                    chain: vec![leader],
                    group: leads.then_some(group),
                    members: Vec::new(),
                    unproven: Vec::new(),
                    signal: i.stop_signal,
                });
            }
            Some(_) => {}
            None => {
                for member in procinfo::in_group(group) {
                    if owned(&member, token, i.started_at_ms, exempt) {
                        others.push(member);
                    } else {
                        unverified.push(serde_json::json!({
                            "service": name,
                            "pid": member.pid,
                            "reason": "in the service's recorded process group, but not shown to be this checkout's (no token for it, or started before the service); not signalled",
                        }));
                    }
                }
            }
        }
    }
    // Beyond the recorded groups only while it is known what is a command.
    if let (true, Some(token), None) = (any, token, &exempt.blind) {
        let covered = |p: &procinfo::Process, strays: &[crate::stopping::Stray], others: &[procinfo::Process]| {
            strays.iter().any(|s| s.chain.iter().any(|c| c.same(p)) || s.group == Some(p.group)) || others.iter().any(|o| o.same(p))
        };
        for p in procinfo::carrying(crate::checkout::TOKEN_VAR, token) {
            if !covered(&p, &strays, &others) && owned(&p, Some(token), earliest.unwrap_or(0), exempt) {
                others.push(p);
            }
        }
    }
    (strays, others, unverified)
}

/// Stop what a supervisor that is no longer running left (see [`scan`]): each verified leader's
/// group with its service's stop signal, each other verified process by pid, `SIGKILL` after
/// [`STOP_GRACE`], every signal bound to the process observed as `crate::stopping` describes.
/// Returns what is still alive, and what could not be shown to be the checkout's (never
/// signalled): nothing new may start while either remains.
///
/// `dir` is the supervisor's directory, inside the Stack state whose command registries name
/// commands never to signal.
pub fn recover(dir: &Path, state: &State, token: Option<&str>) -> Vec<serde_json::Value> {
    let exempt = exempt_for(dir);
    let earliest = state.services.values().filter(|i| i.process.as_ref().is_some_and(Proc::this_boot)).map(|i| i.started_at_ms).min().unwrap_or(0);
    let rescan = |t: &str| {
        if exempt.blind.is_some() {
            return Vec::new();
        }
        crate::stopping::independent(procinfo::carrying(crate::checkout::TOKEN_VAR, t).into_iter().filter(|p| owned(p, Some(t), earliest, &exempt)).collect())
    };
    let rescan_token = token.map(|t| move || rescan(t));
    let rescan_ref: Option<&dyn Fn() -> Vec<crate::stopping::Found>> = rescan_token.as_ref().map(|r| r as &dyn Fn() -> Vec<crate::stopping::Found>);
    // Once a leader is gone its group is looked at member by member, so a second pass stops
    // what stayed in a group whose leader exited during the first; a third finds nothing new.
    for _ in 0..3 {
        let (strays, others, unverified) = scan(state, token, &exempt);
        if strays.is_empty() && others.is_empty() {
            return unverified;
        }
        let left = crate::stopping::stop(&strays, &others, rescan_ref, STOP_GRACE);
        if !left.is_empty() || !unverified.is_empty() {
            return left.into_iter().chain(unverified).collect();
        }
    }
    let (strays, others, unverified) = scan(state, token, &exempt);
    strays.iter().map(|s| serde_json::json!({ "service": s.service, "pid": s.chain[0].pid })).chain(others.iter().map(|p| serde_json::json!({ "pid": p.pid }))).chain(unverified).collect()
}

/// What may still run of the services the supervisor in `dir` recorded while no supervisor
/// runs it (see [`scan`]), nothing signalled: their verified processes and what cannot be shown
/// to be the checkout's. Empty while the supervisor runs (it accounts for its services) and
/// once nothing recorded remains. Data operations refuse while this is not empty.
pub fn unresolved(dir: &Path) -> Vec<serde_json::Value> {
    let state = state_lossy(dir);
    if alive(&state) {
        return Vec::new();
    }
    let token = read_spec(dir).ok().flatten().and_then(|s| s.token);
    let (strays, others, mut unverified) = scan(&state, token.as_deref(), &exempt_for(dir));
    let mut found: Vec<serde_json::Value> = strays.iter().map(|s| serde_json::json!({ "service": s.service, "pid": s.chain[0].pid })).collect();
    found.extend(others.iter().map(|p| serde_json::json!({ "pid": p.pid })));
    found.append(&mut unverified);
    found
}

/// The last `tail` lines `service` wrote, oldest first, keeping only lines read at or after
/// `since` (Unix milliseconds) when given; and whether older output was left unread.
pub fn logs(dir: &Path, service: &str, tail: usize, since: Option<u64>) -> std::io::Result<(Vec<String>, bool)> {
    let path = log_path(dir, service);
    let mut older = path.as_os_str().to_owned();
    older.push(".1");
    let mut chunks: Vec<(String, bool)> = Vec::new();
    let mut budget = LOGS_LIMIT;
    for file in [path.clone(), PathBuf::from(older)] {
        let Ok(mut f) = fs::File::open(&file) else { continue };
        let len = f.metadata()?.len();
        let start = len.saturating_sub(budget);
        f.seek(std::io::SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        f.take(budget).read_to_end(&mut bytes)?;
        budget -= bytes.len() as u64;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if start > 0 {
            // Begin at a whole line.
            text = text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
        }
        chunks.push((text, start > 0));
        if budget == 0 || start > 0 {
            break;
        }
    }
    let cut = chunks.last().is_some_and(|(_, cut)| *cut) || (budget == 0 && chunks.len() == 1);
    let mut lines: Vec<String> = Vec::new();
    for (text, _) in chunks.iter().rev() {
        for line in text.lines() {
            let (secs, rest) = match line.split_once(' ') {
                Some((s, rest)) if s.bytes().all(|b| b.is_ascii_digit()) && !s.is_empty() => (s.parse::<u64>().unwrap_or(0), rest),
                _ => (0, line),
            };
            if since.map_or(true, |at| secs >= at) {
                lines.push(rest.to_string());
            }
        }
    }
    let truncated = cut && lines.len() <= tail;
    let skip = lines.len().saturating_sub(tail);
    Ok((lines.split_off(skip), truncated))
}

// ---- the supervisor ------------------------------------------------------------------------

static WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(signal: libc::c_int) {
    if signal == libc::SIGTERM {
        TERM.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let fd = WAKE.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: write is async-signal-safe; a full pipe already holds a wake-up.
        unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) };
    }
}

/// How long the supervisor may run services without publishing their record before it stops
/// them: nothing may run that no record names.
const PUBLISH_BOUND: Duration = Duration::from_secs(5);
/// Readiness is checked this often at first, then less often (up to [`READY_INTERVAL_MAX`]) for
/// a service still not ready after [`READY_PATIENCE`].
const READY_INTERVAL: Duration = Duration::from_millis(10);
const READY_INTERVAL_MAX: Duration = Duration::from_secs(1);
const READY_PATIENCE: Duration = Duration::from_secs(2);

/// One service process the supervisor runs.
struct Live {
    process: Proc,
    status: &'static str,
    port: u16,
    data_dir: Option<PathBuf>,
    stop_signal: i32,
    ready: Ready,
    env: IndexMap<String, String>,
    cwd: PathBuf,
    started_at_ms: u64,
    spawned: Instant,
    restarts: u32,
    /// When the stop signal was sent, and whether the group was killed since.
    stopping: Option<(Instant, bool)>,
    /// A readiness command running now, by pid and when it must end.
    probe: Option<(u32, Instant)>,
    /// When readiness is next checked, and how long after that the one after.
    next_check: Instant,
    interval: Duration,
}

impl Live {
    /// Wait longer before the next readiness check once the service has been starting a while.
    fn checked(&mut self, now: Instant) {
        if now.duration_since(self.spawned) >= READY_PATIENCE {
            self.interval = (self.interval * 2).min(READY_INTERVAL_MAX);
        }
        self.next_check = now + self.interval;
    }
}

/// `stack __supervise <dir>`: run the supervisor of `dir` until it is told to shut down.
pub fn main_supervise(dir: &Path) -> i32 {
    // Forked once more, so the command that started this has nothing to wait for. Nothing else
    // runs in this process yet, so forking is safe.
    // SAFETY: fork in a single-threaded process.
    match unsafe { libc::fork() } {
        -1 => return 1,
        0 => {}
        _ => return 0,
    }
    match supervise(dir) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("stack supervisor {}: {e}", dir.display());
            1
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// What identifies the spec file's content without reading it: size and modification time.
fn spec_stamp(dir: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = fs::metadata(spec_path(dir)).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn supervise(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir.join("logs"))?;
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(dir.join("lock"))?;
    {
        use std::os::fd::AsRawFd;
        // Another supervisor of this checkout holds it for life; a reader probing it holds it
        // for an instant, so try for a while before concluding the former.
        let until = Instant::now() + Duration::from_secs(2);
        // SAFETY: flock on a descriptor this function owns.
        while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            if Instant::now() >= until {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let (wake_read, wake_write) = wake_pipe()?;
    WAKE.store(wake_write, std::sync::atomic::Ordering::SeqCst);
    // SAFETY: plain signal disposition changes at startup, before any thread exists.
    unsafe {
        for sig in [libc::SIGCHLD, libc::SIGHUP, libc::SIGTERM] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGPIPE] {
            libc::signal(sig, libc::SIG_IGN);
        }
    }
    let me = Proc::observe(std::process::id());
    // What a supervisor before this one left running is stopped first: this one cannot wait
    // for processes it did not start. What cannot be shown stopped keeps everything as it is.
    let previous = state_lossy(dir);
    let mut spec = match read_spec(dir) {
        Ok(spec) => spec.unwrap_or_default(),
        Err(e) => return Err(std::io::Error::other(e.message)),
    };
    let left = recover(dir, &previous, spec.token.as_deref());
    if !left.is_empty() {
        return Err(std::io::Error::other(format!("processes an earlier supervisor left could not be confirmed stopped, so nothing was started: {}", serde_json::Value::Array(left))));
    }
    let mut stopped: IndexMap<String, Instance> = settled(&previous.services);
    let mut live: IndexMap<String, Live> = IndexMap::new();
    let mut retry_at: IndexMap<String, Instant> = IndexMap::new();
    let mut started_seq: IndexMap<String, u64> = IndexMap::new();
    // Readiness probes of services that stopped or exited, killed with their groups until reaped.
    let mut orphan_probes: Vec<u32> = Vec::new();
    let mut writers = LogWriters::default();
    let mut applied = previous.applied;
    let mut stamp = spec_stamp(dir);
    let mut last_written: Option<State> = None;
    let mut unpublished: Option<Instant> = None;
    let mut forced = false;
    let mut first = true;
    loop {
        let woken = drain(wake_read) || first;
        first = false;
        cap_own_log(dir);
        // The spec is read again only when a command asked (a wake-up) or the file changed.
        let now_stamp = spec_stamp(dir);
        if woken || now_stamp != stamp {
            stamp = now_stamp;
            match read_spec(dir) {
                Ok(Some(read)) => spec = read,
                Ok(None) => spec.shutdown = true,
                Err(e) => eprintln!("stack supervisor: {}; keeping the last spec read", e.message),
            }
        }
        let shutting = spec.shutdown || forced || TERM.load(std::sync::atomic::Ordering::SeqCst);
        let now = Instant::now();
        // Exits first, so a stop or start below sees them. A leader that exited is killed with
        // its group while it is still unreaped, and only then reaped.
        let exited: Vec<String> = live.iter().filter(|(_, l)| has_exited(l.process.pid)).map(|(n, _)| n.clone()).collect();
        for name in exited {
            let l = live.shift_remove(&name).expect("listed");
            let how = finish(l.process.pid);
            let was_stopping = l.stopping.is_some();
            orphan_probes.extend(l.probe.map(|(pid, _)| pid));
            let retries = spec.services.get(&name).map_or(0, |s| s.retries);
            let wanted = !shutting && spec.services.get(&name).is_some_and(|s| s.want);
            if !was_stopping && wanted && l.restarts < retries {
                let delay = Duration::from_secs(1u64 << l.restarts.min(5));
                retry_at.insert(name.clone(), now + delay);
                eprintln!("stack supervisor: {name} exited ({how}); starting it again in {}s", delay.as_secs());
            }
            stopped.insert(
                name.clone(),
                Instance {
                    status: if was_stopping { "stopped" } else { "failed" }.into(),
                    process: None,
                    group: None,
                    port: l.port,
                    data_dir: l.data_dir,
                    stop_signal: l.stop_signal,
                    started_at_ms: l.started_at_ms,
                    exit: Some(how),
                    restarts: l.restarts,
                },
            );
        }
        // Probes nobody waits for any more: their groups are killed while they are unreaped.
        orphan_probes.retain(|pid| {
            if has_exited(*pid) {
                finish(*pid);
                return false;
            }
            signal_group(*pid, libc::SIGKILL);
            true
        });
        // Stops, and escalation, while each leader is still unreaped.
        for (name, l) in live.iter_mut() {
            let wanted = !shutting && spec.services.get(name).is_some_and(|s| s.want);
            match l.stopping {
                None if !wanted => {
                    signal_group(l.process.pid, l.stop_signal);
                    l.stopping = Some((now, false));
                    l.status = "stopping";
                    if let Some((pid, _)) = l.probe.take() {
                        signal_group(pid, libc::SIGKILL);
                        orphan_probes.push(pid);
                    }
                }
                Some((since, false)) if since.elapsed() >= STOP_GRACE => {
                    signal_group(l.process.pid, libc::SIGKILL);
                    l.stopping = Some((since, true));
                }
                _ => {}
            }
        }
        // Starts.
        if !shutting {
            for (name, s) in &spec.services {
                if !s.want {
                    retry_at.shift_remove(name);
                    continue;
                }
                // A request a running instance satisfies is consumed by it: when it later ends
                // on its own, nothing asked for another. One that is stopping satisfies nothing:
                // the request stays, and starts the service once the stop completed.
                if let Some(l) = live.get(name) {
                    if l.stopping.is_none() {
                        started_seq.insert(name.clone(), s.start_seq);
                    }
                    continue;
                }
                if retry_at.get(name).is_some_and(|at| now < *at) {
                    continue;
                }
                // A service that ended on its own starts again only on a retry that is due, or
                // when a command asks for it again (see `ServiceSpec::start_seq`).
                let restarts = match retry_at.shift_remove(name) {
                    Some(_) => stopped.get(name).map_or(0, |i| i.restarts + 1),
                    None if started_seq.get(name).is_some_and(|n| *n >= s.start_seq) => continue,
                    None => 0,
                };
                started_seq.insert(name.clone(), s.start_seq);
                let started_at_ms = now_ms();
                match spawn_service(name, s, writers.sender(dir, name)) {
                    Ok(process) => {
                        stopped.shift_remove(name);
                        live.insert(
                            name.clone(),
                            Live {
                                process,
                                status: if s.ready == Ready::Spawned { "running" } else { "starting" },
                                port: s.port,
                                data_dir: s.data_dir.clone(),
                                stop_signal: s.stop_signal,
                                ready: s.ready.clone(),
                                env: s.env.clone(),
                                cwd: s.cwd.clone(),
                                started_at_ms,
                                spawned: now,
                                restarts,
                                stopping: None,
                                probe: None,
                                next_check: now,
                                interval: READY_INTERVAL,
                            },
                        );
                    }
                    Err(e) => {
                        writers.line(dir, name, &format!("stack: cannot start {name}: {e}"));
                        stopped.insert(
                            name.clone(),
                            Instance {
                                status: "failed".into(),
                                process: None,
                                group: None,
                                port: s.port,
                                data_dir: s.data_dir.clone(),
                                stop_signal: s.stop_signal,
                                started_at_ms,
                                exit: Some(format!("not started: {e}")),
                                restarts,
                            },
                        );
                    }
                }
            }
        }
        // Readiness, each service at its own pace.
        let now = Instant::now();
        for l in live.values_mut().filter(|l| l.status == "starting") {
            match l.ready.clone() {
                Ready::Spawned => l.status = "running",
                Ready::Port { port } => {
                    if now >= l.next_check {
                        if std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(50)).is_ok() {
                            l.status = "running";
                        } else {
                            l.checked(now);
                        }
                    }
                }
                Ready::Command { command } => match l.probe {
                    Some((pid, until)) if has_exited(pid) => {
                        l.probe = None;
                        if finish_status(pid) == Some(0) {
                            l.status = "running";
                        } else {
                            l.checked(now);
                        }
                        let _ = until;
                    }
                    Some((pid, until)) if now >= until => {
                        signal_group(pid, libc::SIGKILL);
                        orphan_probes.push(pid);
                        l.probe = None;
                        l.checked(now);
                    }
                    Some(_) => {}
                    None if now >= l.next_check => match spawn_probe(&command, &l.env, &l.cwd) {
                        Ok(pid) => l.probe = Some((pid, now + READY_CMD_TIMEOUT)),
                        Err(_) => l.checked(now),
                    },
                    None => {}
                },
            }
        }
        applied = applied.max(spec.revision);
        let done = shutting && live.is_empty() && orphan_probes.is_empty();
        let mut services = stopped.clone();
        for (name, l) in &live {
            services.insert(
                name.clone(),
                Instance {
                    status: l.status.into(),
                    process: Some(l.process.clone()),
                    group: Some(l.process.pid),
                    port: l.port,
                    data_dir: l.data_dir.clone(),
                    stop_signal: l.stop_signal,
                    started_at_ms: l.started_at_ms,
                    exit: None,
                    restarts: l.restarts,
                },
            );
        }
        // Manifest order, as commands list them.
        let order: Vec<&String> = spec.services.keys().collect();
        services.sort_by_key(|name, _| order.iter().position(|n| *n == name).unwrap_or(usize::MAX));
        let state = State { supervisor: if done { None } else { me.clone() }, applied, services, project: spec.project.clone(), token: spec.token.clone() };
        if last_written.as_ref() != Some(&state) {
            match write_state(dir, &state) {
                Ok(()) => {
                    last_written = Some(state);
                    unpublished = None;
                }
                Err(e) => {
                    if unpublished.is_none() {
                        eprintln!("stack supervisor: cannot write {}: {e}", state_path(dir).display());
                    }
                    let since = *unpublished.get_or_insert(now);
                    // Running what no record names is not allowed for long: stop everything.
                    if !forced && !live.is_empty() && since.elapsed() >= PUBLISH_BOUND {
                        eprintln!("stack supervisor: {} is still not written after {}s; stopping every service", state_path(dir).display(), PUBLISH_BOUND.as_secs());
                        forced = true;
                    }
                }
            }
        }
        if done {
            return match unpublished {
                Some(_) => Err(std::io::Error::other(format!("could not write {}", state_path(dir).display()))),
                None => Ok(()),
            };
        }
        // Sleep until the next thing due, at most a second: readiness checks, probes, retries,
        // escalations, an unpublished record.
        let mut due = Duration::from_secs(1);
        let at = |t: Instant| t.saturating_duration_since(Instant::now());
        for l in live.values() {
            if l.status == "starting" {
                due = due.min(match l.probe {
                    Some(_) => Duration::from_millis(10),
                    None => at(l.next_check),
                });
            }
            if let Some((since, false)) = l.stopping {
                due = due.min(at(since + STOP_GRACE)).min(Duration::from_millis(50));
            }
        }
        for t in retry_at.values() {
            due = due.min(at(*t));
        }
        if !orphan_probes.is_empty() {
            due = due.min(Duration::from_millis(10));
        }
        if unpublished.is_some() {
            due = due.min(Duration::from_millis(100));
        }
        wait(wake_read, due.as_millis().max(1) as i32);
    }
}

fn wake_pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0; 2];
    // SAFETY: pipe writes two descriptors into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fcntl on descriptors just created.
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok((fds[0], fds[1]))
}

/// Empty the wake-up pipe. Whether anything was in it.
fn drain(fd: i32) -> bool {
    let mut buf = [0u8; 64];
    let mut any = false;
    // SAFETY: reads into a local buffer from a nonblocking descriptor.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {
        any = true;
    }
    any
}

fn wait(fd: i32, timeout_ms: i32) {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: poll on one valid descriptor.
    unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
}

/// Whether child `pid` has exited, without reaping it (`WNOWAIT`): until it is reaped its pid,
/// and so its group's number, cannot be handed to another process. A pid that is not a child
/// any more (never expected: only [`finish`] reaps) counts as exited too.
fn has_exited(pid: u32) -> bool {
    !matches!(child_state(pid), ChildState::Running)
}

enum ChildState {
    Running,
    /// Exited, not reaped: its pid is still held.
    Exited,
    /// Not a child of this process.
    Gone,
}

fn child_state(pid: u32) -> ChildState {
    // SAFETY: waitid writes only the provided struct.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let r = libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        match (r, info.si_pid()) {
            (0, 0) => ChildState::Running,
            (0, _) => ChildState::Exited,
            _ => ChildState::Gone,
        }
    }
}

/// Kill what child `pid` left in its process group while its exited leader still holds the
/// group's number, then reap it. How it ended.
fn finish(pid: u32) -> String {
    if !matches!(child_state(pid), ChildState::Exited) {
        return "gone".into();
    }
    signal_group(pid, libc::SIGKILL);
    let mut status = 0;
    // SAFETY: waitpid on a child of this process that has exited.
    let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    if r < 0 {
        return "gone".into();
    }
    if libc::WIFEXITED(status) {
        format!("exit code {}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("signal {}", libc::WTERMSIG(status))
    } else {
        "ended".into()
    }
}

/// [`finish`], as an exit code (`None` for anything but a normal exit).
fn finish_status(pid: u32) -> Option<i32> {
    if !matches!(child_state(pid), ChildState::Exited) {
        return None;
    }
    signal_group(pid, libc::SIGKILL);
    let mut status = 0;
    // SAFETY: waitpid on a child of this process that has exited.
    let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    (r > 0 && libc::WIFEXITED(status)).then(|| libc::WEXITSTATUS(status))
}

/// Signal the process group `leader` leads. Only called while the leader is an unreaped child
/// of this process (running, or exited and not yet reaped), so the group's number cannot be
/// another's.
fn signal_group(leader: u32, signal: i32) {
    // SAFETY: kill only sends a signal.
    unsafe { libc::kill(-(leader as libc::pid_t), signal) };
}

/// Reset in a child before it runs its program: what the supervisor ignores or handles must
/// not be ignored by a service. On Linux the child gets `stop_signal` when the supervisor
/// (`parent`) dies; a supervisor that died before that was registered is noticed at once.
fn child_setup(stop_signal: i32, parent: libc::pid_t) -> std::io::Result<()> {
    // SAFETY: only async-signal-safe calls, in a forked child before exec.
    unsafe {
        if libc::setsid() < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGPIPE, libc::SIGHUP, libc::SIGTERM, libc::SIGCHLD] {
            libc::signal(sig, libc::SIG_DFL);
        }
        #[cfg(target_os = "linux")]
        if libc::prctl(
            libc::PR_SET_PDEATHSIG,
            stop_signal as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = stop_signal;
        if libc::getppid() != parent {
            return Err(std::io::Error::other("the supervisor exited before the service started"));
        }
    }
    Ok(())
}

/// A pipe whose ends are closed on exec: the read end for the supervisor, the write end for the
/// child's output (which `Command` duplicates onto its stdout and stderr).
fn output_pipe() -> std::io::Result<(fs::File, fs::File)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: pipe writes two descriptors into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fcntl on descriptors just created.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors were just created and are owned by nothing else.
    Ok(unsafe { (fs::File::from_raw_fd(fds[0]), fs::File::from_raw_fd(fds[1])) })
}

fn spawn_service(name: &str, s: &ServiceSpec, log: std::sync::mpsc::Sender<Vec<u8>>) -> std::io::Result<Proc> {
    use std::os::unix::process::CommandExt;
    let (program, args) = s.argv.split_first().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no command"))?;
    // Looked up on the service's PATH, not the supervisor's (which is empty).
    let program = match program.contains('/') {
        true => PathBuf::from(program),
        false => s
            .env
            .get("PATH")
            .and_then(|p| std::env::split_paths(p).map(|d| d.join(program)).find(|p| p.is_file()))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{program} is not on the service's PATH")))?,
    };
    let (reader, writer) = output_pipe()?;
    let mut command = std::process::Command::new(program);
    let stop_signal = s.stop_signal;
    let parent = std::process::id() as libc::pid_t;
    command
        .args(args)
        .env_clear()
        .envs(&s.env)
        .current_dir(&s.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    // SAFETY: child_setup makes only async-signal-safe calls.
    unsafe { command.pre_exec(move || child_setup(stop_signal, parent)) };
    let child = command.spawn()?;
    // The write ends are the child's now.
    drop(command);
    let pid = child.id();
    // Reaped by pid in the loop; the handle has nothing more to do.
    drop(child);
    let process = Proc::observe(pid).unwrap_or(Proc { pid, started: 0, boot: procinfo::boot_id() });
    let _ = name;
    std::thread::spawn(move || pump(reader, log));
    Ok(process)
}

/// One writer per service log, for the supervisor's life: every process of the service (old
/// generations still draining included) sends its lines here, so only one thread writes the
/// file and rotates it.
#[derive(Default)]
struct LogWriters(IndexMap<String, std::sync::mpsc::Sender<Vec<u8>>>);

impl LogWriters {
    fn sender(&mut self, dir: &Path, service: &str) -> std::sync::mpsc::Sender<Vec<u8>> {
        self.0
            .entry(service.to_string())
            .or_insert_with(|| {
                let (send, receive) = std::sync::mpsc::channel::<Vec<u8>>();
                let path = log_path(dir, service);
                std::thread::spawn(move || write_log(receive, &path));
                send
            })
            .clone()
    }

    /// A line of the supervisor's own about `service`, in its log.
    fn line(&mut self, dir: &Path, service: &str, line: &str) {
        let _ = self.sender(dir, service).send(format!("{} {line}\n", now_ms()).into_bytes());
    }
}

/// Copy a service's output to its log writer, each line prefixed with when it was read (Unix
/// ms). Ends when every process of the service closed its output.
fn pump(reader: fs::File, log: std::sync::mpsc::Sender<Vec<u8>>) {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.by_ref().take(64 * 1024).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        let mut out = format!("{} ", now_ms()).into_bytes();
        out.extend_from_slice(&line);
        if log.send(out).is_err() {
            return;
        }
    }
}

/// The only writer of one service's log: appends what pumps send, and rotates the file (one
/// older file kept) once it passes [`LOG_ROTATE_BYTES`].
fn write_log(receive: std::sync::mpsc::Receiver<Vec<u8>>, log: &Path) {
    let open = |log: &Path| fs::OpenOptions::new().create(true).append(true).open(log);
    let mut file = open(log).ok();
    for chunk in receive {
        let Some(f) = file.as_mut() else {
            file = open(log).ok();
            continue;
        };
        let _ = f.write_all(&chunk);
        if f.metadata().is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
            let mut older = log.as_os_str().to_owned();
            older.push(".1");
            let _ = fs::rename(log, PathBuf::from(older));
            file = open(log).ok();
        }
    }
}

/// Run a readiness command as Stack's own check runs it: `sh` from the service's PATH (none
/// there, and the check cannot run, so the service is not ready).
fn spawn_probe(command: &str, env: &IndexMap<String, String>, cwd: &Path) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let shell = match env.get("PATH") {
        Some(path) => std::env::split_paths(path).map(|d| d.join("sh")).find(|p| p.is_file()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no sh on the service's PATH"))?,
        None => PathBuf::from("/bin/sh"),
    };
    let mut sh = std::process::Command::new(shell);
    let parent = std::process::id() as libc::pid_t;
    sh.args(["-c", command])
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: child_setup makes only async-signal-safe calls.
    unsafe { sh.pre_exec(move || child_setup(libc::SIGKILL, parent)) };
    let child = sh.spawn()?;
    Ok(child.id())
}

// ---- presets -------------------------------------------------------------------------------

/// What Stack runs for each service preset (as mise 2026.10.3's presets did, so data they
/// initialised keeps working): the server, its readiness check, the signal that stops it, and
/// whether it keeps data.
pub struct Preset {
    pub name: &'static str,
    /// Its server binary, found on the service's PATH.
    pub server: &'static str,
    pub stop_signal: i32,
    pub keeps_data: bool,
}

pub fn preset(name: &str) -> Option<Preset> {
    let (server, stop_signal, keeps_data) = match name {
        "postgres" => ("postgres", libc::SIGINT, true),
        "redis" => ("redis-server", libc::SIGTERM, true),
        "cockroachdb" => ("cockroach", libc::SIGTERM, true),
        "nats" => ("nats-server", libc::SIGTERM, true),
        "spicedb" => ("spicedb", libc::SIGTERM, true),
        _ => return None,
    };
    let name = match name {
        "postgres" => "postgres",
        "redis" => "redis",
        "cockroachdb" => "cockroachdb",
        "nats" => "nats",
        _ => "spicedb",
    };
    Some(Preset { name, server, stop_signal, keeps_data })
}

/// The ports mise's presets fixed besides the service's own, by role, which Stack now reserves
/// per checkout (as `<service>.<role>`, see [`secondary_roles`]); these are only the defaults
/// for a service whose reservation is missing.
fn default_secondary(role: &str) -> u16 {
    match role {
        "http" => 8080,
        "monitor" => 8222,
        "spicedb_http" => 8443,
        _ => 9090,
    }
}

const SPICEDB_KEY: &str = "mise-dev-key";

/// The further ports a preset listens on besides its own, by role: each is reserved per
/// checkout, so two checkouts can run the preset at once and a readiness check reaches the
/// checkout's own server.
pub fn secondary_roles(preset: &str) -> &'static [&'static str] {
    match preset {
        "cockroachdb" => &["http"],
        "nats" => &["monitor"],
        "spicedb" => &["http", "metrics"],
        _ => &[],
    }
}

/// A preset service's further ports, by role.
pub type Secondary = IndexMap<String, u16>;

fn role(secondary: &Secondary, preset: &str, role: &str) -> u16 {
    secondary.get(role).copied().unwrap_or_else(|| default_secondary(if preset == "spicedb" && role == "http" { "spicedb_http" } else { role }))
}

/// The connection variables a preset exports for its ports, as mise's presets did. A project's
/// own `[env]` value for one of them wins.
pub fn preset_exports(preset: &str, port: u16, secondary: &Secondary) -> Vec<(&'static str, String)> {
    let r = |name: &str| role(secondary, preset, name);
    match preset {
        "postgres" => vec![
            ("DATABASE_URL", format!("postgresql://postgres@127.0.0.1:{port}/postgres")),
            ("PGDATABASE", "postgres".into()),
            ("PGHOST", "127.0.0.1".into()),
            ("PGPORT", port.to_string()),
            ("PGUSER", "postgres".into()),
        ],
        "redis" => vec![("REDIS_URL", format!("redis://127.0.0.1:{port}"))],
        "cockroachdb" => vec![
            ("COCKROACH_HOST", format!("127.0.0.1:{port}")),
            ("COCKROACH_URL", format!("postgresql://root@127.0.0.1:{port}/defaultdb?sslmode=disable")),
            ("CRDB_HTTP_PORT", r("http").to_string()),
        ],
        "nats" => vec![
            ("NATS_URL", format!("nats://127.0.0.1:{port}")),
            ("NATS_MONITOR_PORT", r("monitor").to_string()),
            ("NATS_MONITORING_URL", format!("http://127.0.0.1:{}", r("monitor"))),
        ],
        "spicedb" => vec![
            ("SPICEDB_ENDPOINT", format!("127.0.0.1:{port}")),
            ("SPICEDB_HTTP_PORT", r("http").to_string()),
            ("SPICEDB_METRICS_PORT", r("metrics").to_string()),
            ("SPICEDB_PRESHARED_KEY", SPICEDB_KEY.into()),
        ],
        _ => Vec::new(),
    }
}

/// How the supervisor tells a preset's server is ready. Postgres and Redis: their port, since
/// Stack then verifies each through its protocol; the others: the check mise's presets ran,
/// against this checkout's own ports.
pub fn preset_ready(preset: &str, port: u16, secondary: &Secondary) -> Ready {
    let r = |name: &str| role(secondary, preset, name);
    match preset {
        "cockroachdb" => Ready::Command { command: format!("cockroach sql --insecure --host=127.0.0.1:{port} -e 'SELECT 1'") },
        "nats" => Ready::Command { command: format!("curl -fsS http://127.0.0.1:{}/healthz", r("monitor")) },
        "spicedb" => Ready::Command { command: format!("curl -fsS http://127.0.0.1:{}/healthz", r("http")) },
        _ => Ready::Port { port },
    }
}

/// The server's arguments for `data_dir` and its ports, as mise's presets ran it.
fn server_args(preset: &str, data_dir: &str, port: u16, secondary: &Secondary) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let r = |name: &str| role(secondary, preset, name);
    let port_s = port.to_string();
    match preset {
        // `-k ""`: no Unix socket, only TCP on loopback.
        "postgres" => s(&["-D", data_dir, "-p", &port_s, "-h", "127.0.0.1", "-k", ""]),
        "redis" => s(&["--bind", "127.0.0.1", "--port", &port_s, "--dir", data_dir, "--appendonly", "yes"]),
        "cockroachdb" => vec![
            "start-single-node".into(),
            "--insecure".into(),
            format!("--store={data_dir}"),
            format!("--listen-addr=127.0.0.1:{port}"),
            format!("--http-addr=127.0.0.1:{}", r("http")),
        ],
        "nats" => s(&["--addr", "127.0.0.1", "--port", &port_s, "--http_port", &r("monitor").to_string(), "--jetstream", "--store_dir", data_dir]),
        _ => vec![
            "serve".into(),
            "--grpc-addr".into(),
            format!("127.0.0.1:{port}"),
            "--http-addr".into(),
            format!("127.0.0.1:{}", r("http")),
            "--http-enabled".into(),
            "--metrics-addr".into(),
            format!("127.0.0.1:{}", r("metrics")),
            "--grpc-preshared-key".into(),
            SPICEDB_KEY.into(),
            "--datastore-engine".into(),
            "memory".into(),
        ],
    }
}

/// The version of the preset's server first on PATH, from its version output
/// (`postgres (PostgreSQL) 17.12`, `Redis server v=8.10.3`, `Build Tag: v25.4.20`,
/// `nats-server: v2.15.3`, `spicedb v1.57.1`).
fn server_version(p: &Preset) -> std::result::Result<String, String> {
    let args: &[&str] = match p.name {
        "cockroachdb" | "spicedb" => &["version"],
        _ => &["--version"],
    };
    let out = std::process::Command::new(p.server)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", p.server))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    parse_version(&text).ok_or_else(|| format!("cannot read a version from `{} {}`: {:?}", p.server, args.join(" "), text.trim()))
}

/// The first dotted version number in `text` (`17.12`, `8.10.3`), without a leading `v`.
pub fn parse_version(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '=' || c == ',').map(|w| w.trim_start_matches('v')).find_map(|word| {
        let mut parts = word.split('.');
        let major = parts.next()?;
        let numeric = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        let rest: Vec<&str> = parts.collect();
        (numeric(major) && !rest.is_empty() && rest.iter().all(|p| numeric(p))).then(|| word.to_string())
    })
}

/// The major component of the first version number in `text`.
pub fn parse_major(text: &str) -> Option<String> {
    parse_version(text).and_then(|v| v.split('.').next().map(str::to_string))
}

/// Make `data_dir` hold `p`'s data for the server on PATH, as mise's presets did: a fresh
/// directory is initialised (Postgres: a copy of the template for this server and environment
/// under `templates`, see [`crate::pgtemplate`], else `initdb -U postgres --auth=trust`, into a
/// sibling directory renamed into place, so a failed one leaves nothing half made) and records
/// the preset and major version; existing data must record the same, or it is refused, kept as
/// it is. A server whose version is not exactly the pinned one (the pinned release's install
/// was not found on the service's PATH, so another release was) initialises and opens nothing.
pub fn init_data(p: &Preset, data_dir: &Path, pinned: Option<&str>, templates: Option<&Path>) -> std::result::Result<(), String> {
    let version = server_version(p)?;
    if let Some(pinned) = pinned.filter(|v| parse_version(v).as_deref() == Some(*v)) {
        if version != pinned {
            let tool = crate::provider::mise::preset_tool(p.name).unwrap_or(p.server);
            return Err(format!(
                "the {} this service found is version {version}, but stack.lock pins {pinned}: no install of {tool} {pinned} is on the stack's PATH, so another release was found. Run `stack install` if it is not installed (`mise where {tool}@{pinned}` says), then `stack up`",
                p.server
            ));
        }
    }
    let major = version.split('.').next().unwrap_or_default().to_string();
    let marker = format!("{}:{}", p.name, major);
    let fresh = match fs::read_dir(data_dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(format!("cannot read {}: {e}", data_dir.display())),
    };
    if !fresh {
        return match fs::read_to_string(data_dir.join(VERSION_FILE)) {
            Ok(stored) if stored.trim() == marker => Ok(()),
            Ok(stored) => Err(format!(
                "incompatible {} data in {} (stored {}, current {marker}); migrate or reset it explicitly",
                p.name,
                data_dir.display(),
                stored.trim()
            )),
            Err(_) => Err(format!("data in {} was not initialized completely; inspect it and migrate or reset it explicitly", data_dir.display())),
        };
    }
    let parent = data_dir.parent().ok_or_else(|| format!("{} has no parent directory", data_dir.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    if p.name != "postgres" {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir).map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        return fs::write(data_dir.join(VERSION_FILE), &marker).map_err(|e| format!("cannot write {}: {e}", data_dir.display()));
    }
    let staging = tempfile::Builder::new().prefix(".stack-init-").tempdir_in(parent).map_err(|e| format!("cannot create a directory in {}: {e}", parent.display()))?;
    let target = staging.path().join("data");
    let env = |name: &str| std::env::var_os(name);
    for note in crate::pgtemplate::make(templates, &marker, &target, &env)? {
        eprintln!("stack: {note}");
    }
    if data_dir.exists() {
        // Empty, as checked above.
        fs::remove_dir(data_dir).map_err(|e| format!("cannot replace {}: {e}", data_dir.display()))?;
    }
    fs::rename(&target, data_dir).map_err(|e| format!("cannot move the new cluster to {}: {e}", data_dir.display()))
}

/// `stack __preset <preset> <data dir> <port> [<pinned version>|-] [<role>=<port>...]`:
/// initialise the data if needed and become the preset's server. Errors go to stderr, which is
/// the service's log.
pub fn main_preset(args: &[std::ffi::OsString]) -> i32 {
    use std::os::unix::process::CommandExt;
    let usage = || {
        eprintln!("usage: stack {PRESET} <preset> <data dir> <port> [<pinned version>|-] [<role>=<port>...] [{TEMPLATES_ARG}<dir>]");
        2
    };
    let [preset_name, data_dir, port, rest @ ..] = args else { return usage() };
    let pinned = rest.first().and_then(|p| p.to_str()).filter(|p| *p != "-" && !p.contains('='));
    let mut secondary = Secondary::new();
    let mut templates = None;
    for arg in rest.iter().skip(1) {
        if let Some(dir) = arg.to_str().and_then(|a| a.strip_prefix(TEMPLATES_ARG)) {
            templates = Some(PathBuf::from(dir));
            continue;
        }
        let Some((role, port)) = arg.to_str().and_then(|a| a.split_once('=')) else { return usage() };
        let Ok(port) = port.parse::<u16>() else { return usage() };
        secondary.insert(role.to_string(), port);
    }
    let Some(p) = preset_name.to_str().and_then(preset) else {
        eprintln!("stack: unknown preset {preset_name:?}");
        return 2;
    };
    let Some(port) = port.to_str().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("stack: invalid port {port:?}");
        return 2;
    };
    let dir = PathBuf::from(data_dir);
    if let Err(e) = init_data(&p, &dir, pinned, templates.as_deref()) {
        eprintln!("stack: {e}");
        return 1;
    }
    let err = std::process::Command::new(p.server).args(server_args(p.name, &dir.to_string_lossy(), port, &secondary)).exec();
    eprintln!("stack: cannot run {}: {err}", p.server);
    127
}

/// How `stack __preset` is told where Postgres templates are kept (see [`crate::pgtemplate`]).
pub const TEMPLATES_ARG: &str = "templates=";

/// The argv the supervisor runs for a preset service. `templates`: where Stack keeps Postgres
/// templates, passed to a Postgres service.
pub fn preset_argv(preset: &str, data_dir: &Path, port: u16, pinned: Option<&str>, secondary: &Secondary, templates: Option<&Path>) -> Result<Vec<String>> {
    let exe = std::env::current_exe().map_err(|e| StackError::new("start_failed", format!("cannot find the stack executable: {e}")))?;
    let mut argv = vec![exe.to_string_lossy().into_owned(), PRESET.into(), preset.into(), data_dir.to_string_lossy().into_owned(), port.to_string()];
    argv.push(pinned.unwrap_or("-").to_string());
    argv.extend(secondary.iter().map(|(role, port)| format!("{role}={port}")));
    if let Some(dir) = templates.filter(|_| preset == "postgres") {
        argv.push(format!("{TEMPLATES_ARG}{}", dir.display()));
    }
    Ok(argv)
}

/// `path` (a PATH value) with the directories of `tool`'s pinned `version` first and those of
/// its other versions left out, so a service runs its own pin's server (and `initdb`) when the
/// stack installs several releases of one preset. mise installs each release under
/// `.../installs/<tool>/<version>/`. Unchanged when no directory of the pin is on it.
pub fn pinned_path(path: &str, tool: &str, version: &str) -> String {
    let release = |dir: &Path| -> Option<String> {
        let parts: Vec<&std::ffi::OsStr> = dir.iter().collect();
        parts.windows(3).find(|w| w[0] == "installs" && w[1] == tool).map(|w| w[2].to_string_lossy().into_owned())
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    if !dirs.iter().any(|d| release(d).as_deref() == Some(version)) {
        return path.to_string();
    }
    let (mine, rest): (Vec<PathBuf>, Vec<PathBuf>) = dirs.into_iter().filter(|d| release(d).map_or(true, |v| v == version)).partition(|d| release(d).is_some());
    std::env::join_paths(mine.into_iter().chain(rest)).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

// ---- data directories ----------------------------------------------------------------------

/// Where a preset service of the checkout at `root` keeps its data when nothing recorded one:
/// `<state>/data/<checkout key>/<service>`, keyed by the path as mise's were, so a checkout
/// recreated at a path finds (and moves aside) its predecessor's data.
pub fn new_data_dir(state: &Path, root: &Path, service: &str) -> PathBuf {
    state.join("data").join(crate::state::project_key(root)).join(service)
}

/// mise's state directory, as mise finds it.
fn mise_state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MISE_STATE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(xdg.join("mise"));
    }
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local/state/mise"))
}

/// The data directory mise's preset kept for `service` of the checkout at `root`, when it
/// exists: `<mise state>/daemons/<hash>/data/<service>`, where `<hash>/state.json` names `root`.
pub fn legacy_data_dir(root: &Path, service: &str) -> Option<PathBuf> {
    let daemons = mise_state_dir()?.join("daemons");
    let wanted = root.to_string_lossy();
    for entry in fs::read_dir(&daemons).ok()?.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("state.json")) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        if v["root"].as_str() == Some(wanted.as_ref()) {
            let dir = entry.path().join("data").join(service);
            if dir.exists() {
                return Some(dir);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::process::CommandExt;

    type SeamHook = Box<dyn FnMut(&str)>;

    thread_local! {
        /// Run at each named point of a recovering shutdown (see `seam`).
        static EVIDENCE_SEAM: RefCell<Option<SeamHook>> = RefCell::new(None);
    }

    pub(super) fn at_evidence_seam(at: &'static str) {
        EVIDENCE_SEAM.with(|seam| {
            if let Some(hook) = seam.borrow_mut().as_mut() {
                hook(at);
            }
        });
    }

    /// A spec and state of the checkout at `project` with `token`: `web` asked to run, its
    /// process (if any) and the supervisor's as given, and the state naming its checkout.
    fn record(project: &str, token: &str, process: Option<Proc>, supervisor: Option<Proc>, applied: u64) -> (Spec, State) {
        let service = ServiceSpec {
            argv: vec!["true".into()],
            env: IndexMap::new(),
            cwd: PathBuf::from("/"),
            port: 20001,
            ready: Ready::Spawned,
            stop_signal: libc::SIGTERM,
            data_dir: None,
            retries: 0,
            want: true,
            start_seq: 1,
        };
        let spec = Spec { revision: applied.max(1), shutdown: false, project: Some(project.into()), token: Some(token.into()), services: IndexMap::from([("web".to_string(), service)]) };
        let instance = Instance {
            status: if process.is_some() { "running" } else { "stopped" }.into(),
            group: process.as_ref().map(|p| p.pid),
            process,
            port: 20001,
            data_dir: None,
            stop_signal: libc::SIGTERM,
            started_at_ms: 0,
            exit: None,
            restarts: 0,
        };
        let state = State {
            supervisor,
            applied,
            services: IndexMap::from([("web".to_string(), instance)]),
            project: Some(project.into()),
            token: Some(token.into()),
        };
        (spec, state)
    }

    fn write(dir: &Path, (spec, state): &(Spec, State)) {
        fs::write(spec_path(dir), serde_json::to_vec(spec).unwrap()).unwrap();
        fs::write(state_path(dir), serde_json::to_vec(state).unwrap()).unwrap();
    }

    fn files(dir: &Path) -> (Vec<u8>, Vec<u8>) {
        (fs::read(spec_path(dir)).unwrap(), fs::read(state_path(dir)).unwrap())
    }

    /// A child in a process group of its own: `sleep`, or one that ignores SIGHUP.
    fn child(ignore_hup: bool) -> std::process::Child {
        let script = if ignore_hup { "trap '' HUP; exec sleep 30" } else { "exec sleep 30" };
        let child = std::process::Command::new("sh").args(["-c", script]).process_group(0).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        child
    }

    fn stop(mut child: std::process::Child) -> bool {
        let alive = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        alive
    }

    fn mine() -> (Vec<String>, PathBuf) {
        (vec!["web".to_string()], PathBuf::from("/checkouts/mine"))
    }

    /// Run a recovering shutdown of `/checkouts/mine` with `at` swapping in `foreign`.
    fn recover_swapping(dir: &Path, at: &'static str, foreign: (Spec, State)) -> Result<()> {
        let target = dir.to_path_buf();
        let mut foreign = Some(foreign);
        EVIDENCE_SEAM.with(|seam| {
            *seam.borrow_mut() = Some(Box::new(move |point| {
                if point == at {
                    if let Some(foreign) = foreign.take() {
                        write(&target, &foreign);
                    }
                }
            }))
        });
        let (asked, project) = mine();
        let result = shutdown_accounted(dir, &Expected { project: &project, token: "mine-token", asked: &asked });
        EVIDENCE_SEAM.with(|seam| *seam.borrow_mut() = None);
        result
    }

    fn refused(result: Result<()>, at: &str) {
        let e = result.expect_err(at);
        assert_eq!(e.code, "state_invalid", "{at}: {e:?}");
        assert!(e.message.contains("not this checkout's") || e.message.contains("does not name this checkout") || e.message.contains("was replaced"), "{at}: {e:?}");
    }

    #[test]
    fn a_recovering_shutdown_never_acts_on_a_record_swapped_in_after_its_spec_check() {
        // Swapped in after the spec was checked and before the state is read: the other
        // checkout's live supervisor must not be signalled, nor its files touched.
        for point in ["evidence", "after_spec_check"] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), &record("/checkouts/mine", "mine-token", None, None, 1));
            let theirs = child(false);
            let foreign = record("/checkouts/theirs", "their-token", None, Proc::observe(theirs.id()), 1);
            let result = recover_swapping(dir.path(), point, foreign.clone());
            let after = files(dir.path());
            refused(result, point);
            assert!(stop(theirs), "{point}: the other checkout's supervisor was signalled");
            write(tempfile::tempdir().unwrap().path(), &foreign);
            assert_eq!(after, (serde_json::to_vec(&foreign.0).unwrap(), serde_json::to_vec(&foreign.1).unwrap()), "{point}: its files changed");
        }
    }

    #[test]
    fn a_recovering_shutdown_never_writes_over_a_record_swapped_in_while_it_waits_for_the_acknowledgement() {
        // This checkout's supervisor is alive (it ignores the SIGHUP, so it never acknowledges);
        // while it is waited for, another checkout's record is put in place: a dead supervisor,
        // no services running and an older revision, which a settle would overwrite.
        let dir = tempfile::tempdir().unwrap();
        let ours = child(true);
        write(dir.path(), &record("/checkouts/mine", "mine-token", None, Proc::observe(ours.id()), 1));
        let foreign = record("/checkouts/theirs", "their-token", None, None, 1);
        let result = recover_swapping(dir.path(), "ack_poll", foreign.clone());
        let after = files(dir.path());
        stop(ours);
        refused(result, "ack_poll");
        assert_eq!(after, (serde_json::to_vec(&foreign.0).unwrap(), serde_json::to_vec(&foreign.1).unwrap()), "its files changed");
    }

    #[test]
    fn a_recovering_shutdown_never_signals_another_checkouts_process_found_while_waiting() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &record("/checkouts/mine", "mine-token", Some(Proc { pid: 1, started: 1, boot: Some("an-earlier-boot".into()) }), None, 1));
        let theirs = child(false);
        let foreign = record("/checkouts/theirs", "their-token", Proc::observe(theirs.id()), None, 1);
        let result = recover_swapping(dir.path(), "wait_poll", foreign.clone());
        let after = files(dir.path());
        refused(result, "wait_poll");
        assert!(stop(theirs), "the other checkout's process was signalled");
        assert_eq!(after, (serde_json::to_vec(&foreign.0).unwrap(), serde_json::to_vec(&foreign.1).unwrap()));
    }

    /// Replace `path` (a new file, by rename) with `bytes`, mode 0644.
    fn replace(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        let fresh = path.with_extension("foreign");
        fs::write(&fresh, bytes).unwrap();
        fs::set_permissions(&fresh, fs::Permissions::from_mode(0o644)).unwrap();
        fs::rename(&fresh, path).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// A recovering shutdown whose `point` replaces the file at `path` (`spec.json` or
    /// `state.json`) with another checkout's while the write is staged.
    fn replaced_while_staging(point: &'static str, path_of: fn(&Path) -> PathBuf, foreign: Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        // A dead supervisor and a service from an earlier boot: the state write is reached too.
        write(dir.path(), &record("/checkouts/mine", "mine-token", Some(Proc { pid: 1, started: 1, boot: Some("an-earlier-boot".into()) }), None, 1));
        let path = path_of(dir.path());
        let (target, bytes) = (path.clone(), foreign.clone());
        EVIDENCE_SEAM.with(|seam| *seam.borrow_mut() = Some(Box::new(move |at| if at == point { replace(&target, &bytes) })));
        let (asked, project) = mine();
        let result = shutdown_accounted(dir.path(), &Expected { project: &project, token: "mine-token", asked: &asked });
        EVIDENCE_SEAM.with(|seam| *seam.borrow_mut() = None);
        refused(result, point);
        assert_eq!(fs::read(&path).unwrap(), foreign, "{point}: the replacement was overwritten");
        assert_eq!(mode(&path), 0o644, "{point}: the replacement's mode was changed");
        // Nothing staged is left behind as a record.
        assert!(fs::read_dir(dir.path()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".tmp")));
    }

    #[test]
    fn a_destination_replaced_while_the_write_is_staged_is_never_overwritten() {
        let (spec, state) = record("/checkouts/theirs", "their-token", None, None, 1);
        replaced_while_staging("spec_staged", spec_path, serde_json::to_vec(&spec).unwrap());
        replaced_while_staging("state_staged", state_path, serde_json::to_vec(&state).unwrap());
    }

    #[test]
    fn a_state_without_the_checkouts_identity_is_no_evidence() {
        // What Stack 0.1.23's supervisor wrote: everything but the identity.
        let dir = tempfile::tempdir().unwrap();
        let (spec, mut state) = record("/checkouts/mine", "mine-token", None, None, 1);
        state.project = None;
        state.token = None;
        write(dir.path(), &(spec, state));
        let before = files(dir.path());
        let (asked, project) = mine();
        let e = shutdown_accounted(dir.path(), &Expected { project: &project, token: "mine-token", asked: &asked }).unwrap_err();
        assert!(e.code == "state_invalid" && e.message.contains("does not name this checkout"), "{e:?}");
        assert_eq!(files(dir.path()), before, "a refused recovery wrote");
    }

    #[test]
    fn a_recovering_shutdown_of_this_checkouts_record_settles_it() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &record("/checkouts/mine", "mine-token", Some(Proc { pid: 1, started: 1, boot: Some("an-earlier-boot".into()) }), None, 1));
        let (asked, project) = mine();
        shutdown_accounted(dir.path(), &Expected { project: &project, token: "mine-token", asked: &asked }).unwrap();
        let state = read_state(dir.path()).unwrap().unwrap();
        assert_eq!((state.project.as_deref(), state.token.as_deref()), (Some(Path::new("/checkouts/mine")), Some("mine-token")));
        assert!(state.services["web"].process.is_none() && read_spec(dir.path()).unwrap().unwrap().shutdown);
    }

    #[test]
    fn versions_are_read_by_their_major_component() {
        assert_eq!(parse_major("postgres (PostgreSQL) 17.12").as_deref(), Some("17"));
        assert_eq!(parse_major("Redis server v=8.10.3 sha=00000000:0 malloc=libc").as_deref(), Some("8"));
        assert_eq!(parse_major("Build Tag:        v25.4.20\nBuild Time: 2026/11/03").as_deref(), Some("25"));
        assert_eq!(parse_major("nats-server: v2.15.3").as_deref(), Some("2"));
        assert_eq!(parse_major("spicedb v1.57.1").as_deref(), Some("1"));
        assert_eq!(parse_major("no version here"), None);
        assert_eq!(parse_version("postgres (PostgreSQL) 17.12").as_deref(), Some("17.12"));
        assert_eq!(parse_version("Redis server v=8.10.3 sha=00000000:0").as_deref(), Some("8.10.3"));
    }

    #[test]
    fn each_service_runs_its_own_pin_when_several_releases_are_installed() {
        let path = "/m/installs/postgres/17.6/bin:/m/installs/postgres/18.1/bin:/m/installs/redis/8.2.1/bin:/usr/bin";
        assert_eq!(pinned_path(path, "postgres", "18.1"), "/m/installs/postgres/18.1/bin:/m/installs/redis/8.2.1/bin:/usr/bin");
        assert_eq!(pinned_path(path, "postgres", "17.6"), "/m/installs/postgres/17.6/bin:/m/installs/redis/8.2.1/bin:/usr/bin");
        // A pin with no install on PATH leaves it as it is (the server's version check refuses).
        assert_eq!(pinned_path(path, "postgres", "16.4"), path);
    }

    #[test]
    fn presets_export_what_mise_exported() {
        let none = Secondary::new();
        let pg: IndexMap<_, _> = preset_exports("postgres", 25085, &none).into_iter().collect();
        assert_eq!(pg["DATABASE_URL"], "postgresql://postgres@127.0.0.1:25085/postgres");
        assert_eq!((pg["PGHOST"].as_str(), pg["PGPORT"].as_str(), pg["PGUSER"].as_str()), ("127.0.0.1", "25085", "postgres"));
        assert_eq!(preset_exports("redis", 25282, &none), vec![("REDIS_URL", "redis://127.0.0.1:25282".to_string())]);
        assert!(preset_exports("custom", 1, &none).is_empty());
        assert_eq!(preset_ready("redis", 9, &none), Ready::Port { port: 9 });
        assert!(matches!(preset_ready("nats", 9, &none), Ready::Command { .. }));
        assert_eq!(server_args("postgres", "/d", 5, &none), vec!["-D", "/d", "-p", "5", "-h", "127.0.0.1", "-k", ""]);
        // The further ports are the checkout's own.
        let own: Secondary = [("monitor".to_string(), 21001), ("http".to_string(), 21002), ("metrics".to_string(), 21003)].into_iter().collect();
        assert_eq!(preset_ready("nats", 9, &own), Ready::Command { command: "curl -fsS http://127.0.0.1:21001/healthz".into() });
        assert!(server_args("nats", "/d", 9, &own).contains(&"21001".to_string()));
        let spice = server_args("spicedb", "/d", 9, &own);
        assert!(spice.contains(&"127.0.0.1:21002".to_string()) && spice.contains(&"127.0.0.1:21003".to_string()), "{spice:?}");
        assert!(server_args("cockroachdb", "/d", 9, &own).contains(&"--http-addr=127.0.0.1:21002".to_string()));
        assert!(preset_exports("nats", 9, &own).contains(&("NATS_MONITORING_URL", "http://127.0.0.1:21001".to_string())));
    }

    #[test]
    fn existing_data_is_kept_only_for_the_same_major_version() {
        let root = tempfile::tempdir().unwrap();
        // A preset whose "server" is a script printing a version.
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = bin.join("nats-server");
        fs::write(&script, "#!/bin/sh\necho 'nats-server: v2.15.3'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let p = Preset { name: "nats", server: "placeholder", stop_signal: libc::SIGTERM, keeps_data: true };
        let p = Preset { server: Box::leak(script.to_string_lossy().into_owned().into_boxed_str()), ..p };
        let data = root.path().join("data/nats");
        let e = init_data(&p, &data, Some("3.0.1"), None).unwrap_err();
        assert!(e.contains("is version 2.15.3, but stack.lock pins 3.0.1") && e.contains("mise where nats-server@3.0.1"), "{e}");
        // The same major, another release: refused too.
        let e = init_data(&p, &data, Some("2.15.4"), None).unwrap_err();
        assert!(e.contains("is version 2.15.3, but stack.lock pins 2.15.4"), "{e}");
        assert!(!data.exists(), "nothing initialised by the wrong server");
        init_data(&p, &data, Some("2.15.3"), None).unwrap();
        assert_eq!(fs::read_to_string(data.join(VERSION_FILE)).unwrap(), "nats:2");
        init_data(&p, &data, None, None).unwrap();
        fs::write(data.join(VERSION_FILE), "nats:1").unwrap();
        let e = init_data(&p, &data, None, None).unwrap_err();
        assert!(e.contains("incompatible nats data") && e.contains("stored nats:1, current nats:2"), "{e}");
        fs::remove_file(data.join(VERSION_FILE)).unwrap();
        fs::write(data.join("x"), "").unwrap();
        assert!(init_data(&p, &data, None, None).unwrap_err().contains("not initialized completely"));
    }

    #[test]
    fn a_listing_reads_only_files_and_start_times() {
        let dir = tempfile::tempdir().unwrap();
        let me = procinfo::process(std::process::id()).unwrap();
        let instance = |status: &str, process: Option<Proc>| Instance {
            status: status.into(),
            process,
            group: None,
            port: 20001,
            data_dir: None,
            stop_signal: libc::SIGTERM,
            started_at_ms: 0,
            exit: None,
            restarts: 0,
        };
        let live = Proc { pid: me.pid, started: me.started, boot: procinfo::boot_id() };
        let reused = Proc { pid: me.pid, started: me.started + 1, boot: procinfo::boot_id() };
        let state = State {
            supervisor: Some(live.clone()),
            applied: 1,
            services: [("a".to_string(), instance("running", Some(live.clone()))), ("b".to_string(), instance("running", Some(reused.clone())))].into_iter().collect(),
            project: None,
            token: None,
        };
        write_state(dir.path(), &state).unwrap();
        let listed = daemons(dir.path(), &[("c".into(), 20003, None)]);
        assert_eq!((listed[0].status.as_str(), listed[0].pid), ("running", Some(me.pid)));
        assert_eq!((listed[1].status.as_str(), listed[1].pid), ("failed", None), "a reused pid is not the service");
        assert_eq!((listed[2].name.as_str(), listed[2].status.as_str()), ("c", "available"));
        // Without a live supervisor, a live service is an orphan.
        write_state(dir.path(), &State { supervisor: Some(reused), ..state.clone() }).unwrap();
        assert_eq!(daemons(dir.path(), &[])[0].status, "orphaned");
        // A record written with this boot's id, as another command reads it, names the process.
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "kern.bootsessionuuid"]).output().unwrap();
            let same = Proc { boot: Some(String::from_utf8(out.stdout).unwrap().trim().to_string()), ..live.clone() };
            assert!(same.alive(), "{same:?}");
        }
        // A record from another boot, or one without a boot, names nothing.
        for boot in [Some("another-boot".to_string()), None] {
            let elsewhere = Proc { boot, ..live.clone() };
            assert!(!elsewhere.alive());
            let state = State { supervisor: Some(elsewhere.clone()), applied: 1, services: [("a".to_string(), instance("running", Some(elsewhere)))].into_iter().collect(), project: None, token: None };
            write_state(dir.path(), &state).unwrap();
            assert_eq!((daemons(dir.path(), &[])[0].status.as_str(), daemons(dir.path(), &[])[0].pid), ("stopped", None));
            assert!(unresolved(dir.path()).is_empty(), "nothing of another boot is a candidate");
        }
    }

    #[test]
    fn logs_keep_the_last_lines_since_a_second() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::write(log_path(dir.path(), "db"), "100 one\n200 two\n300 three\nstray\n").unwrap();
        assert_eq!(logs(dir.path(), "db", 2, None).unwrap(), (vec!["three".to_string(), "stray".to_string()], false));
        assert_eq!(logs(dir.path(), "db", 10, Some(200)).unwrap().0, vec!["two", "three"]);
        assert!(logs(dir.path(), "none", 10, None).unwrap().0.is_empty());
    }
}
