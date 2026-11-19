//! The commands Stack starts for a checkout (`exec`, `run`, and the MCP server's `stack_exec`
//! and `stack_run`), as `status` and `down` report them. Nothing here signals anything.
//!
//! **The registry.** Every such command is registered before it is spawned and removed once it
//! ended, in `<state>/commands/<identity>.json`, keyed by the checkout's identity (its
//! directory's device, inode and creation time, see [`crate::checkout::identity`]): not by its
//! path, so a move keeps it and a directory recreated at the path has another; and not in the
//! session record, so it exists without a session and outlives `down`. An entry names the Stack
//! process that runs the command (its coordinator) and, once spawned, the command itself, each
//! by PID and start time, with how it was started (`exec`, `run <task>`, through MCP) and the
//! program's name, never its arguments. An entry is live while its command is that very process
//! (or, before the spawn, while its coordinator is), so a PID handed to another process later
//! never counts. Entries are read and written under `<identity>.lock`; commands register while
//! they plan, under the project lock `down` holds throughout, so none can register before
//! `down` looks and start after it.
//!
//! **The mark.** Every command also gets the checkout's command mark
//! ([`crate::checkout::COMMAND_VAR`]), which what it starts inherits unless it clears its
//! environment. `down` (not `status`, which reads only the registry) also searches every
//! process's environment for it, to find what a command left running after it ended, also in a
//! process group or session of its own; it searches until two passes agree, so a process that
//! forks and exits while it searches does not hide its child. Not found that way: a process
//! whose environment cannot be read (another user's; on macOS, Apple's own binaries such as
//! `/bin/sh` and `/bin/sleep`), or that cleared it.
//!
//! **Never stopped.** A process that is a registered command or coordinator of any checkout, or
//! carries a command mark, is never signalled by `down`, whatever else its environment holds
//! (such as the checkout's service token, when a service ran `stack exec`): see [`Exempt`].

use crate::error::{io_error, Result, StackError};
use crate::procinfo::{self, Process};
use crate::state::{write_json_unsynced, FileLock};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// A process as observed: its PID, start time (see [`Process::started`]) and the boot it ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub pid: u32,
    pub started: u64,
    /// The boot it was observed in (see [`procinfo::boot_id`]); Linux start times count from
    /// boot, so a record from another boot names nothing that runs now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
}

impl Observed {
    fn of(p: &Process) -> Self {
        Self { pid: p.pid, started: p.started, boot: procinfo::boot_id() }
    }

    /// The process observed, while it is still that process.
    fn current(&self) -> Option<Process> {
        if self.boot.is_some() && self.boot != procinfo::boot_id() {
            return None;
        }
        Process { pid: self.pid, parent: 0, group: 0, started: self.started }.current()
    }

    fn is(&self, p: &Process) -> bool {
        self.pid == p.pid && self.started == p.started && self.current().is_some()
    }
}

/// One registered command.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    /// `exec`, `run <task>`, `mcp stack_exec` or `mcp stack_run <task>`.
    started_by: String,
    /// The program's file name.
    program: String,
    coordinator: Observed,
    /// Set once spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<Observed>,
    registered_at_ms: u64,
}

/// What an entry says now.
enum State {
    /// Its command (or, before the spawn, its coordinator) runs.
    Live(Process),
    /// Its coordinator is gone and its command's process was never recorded: whether that
    /// command runs cannot be known. It counts until removed by hand (see [`unrecorded`]).
    Unrecorded,
    Over,
}

impl Entry {
    fn state(&self) -> State {
        match (&self.command, self.coordinator.current()) {
            (Some(command), _) => command.current().map_or(State::Over, State::Live),
            (None, Some(coordinator)) => State::Live(coordinator),
            (None, None) => State::Unrecorded,
        }
    }

    fn live(&self) -> Option<Process> {
        match self.state() {
            State::Live(p) => Some(p),
            _ => None,
        }
    }
}

fn dir(state: &Path) -> PathBuf {
    state.join("commands")
}

/// The registry file of the checkout at `root`, and its lock.
fn files(state: &Path, root: &Path) -> Result<(PathBuf, PathBuf)> {
    let id = crate::checkout::identity(root)
        .ok_or_else(|| StackError::new("state_invalid", format!("cannot identify the checkout directory {} to register its commands", root.display())))?;
    Ok((dir(state).join(format!("{id}.json")), dir(state).join(format!("{id}.lock"))))
}

/// The entries in `path`; none when it does not exist. One that cannot be read is an error:
/// what it held is unknown.
fn read(path: &Path) -> Result<Vec<Entry>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            StackError::new("state_invalid", format!("the command registry {} cannot be read: {e}", path.display()))
                .hint("check that no command Stack started for this checkout still runs (`ps`), then delete the file: what it held cannot be read")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

/// The identity of every checkout that ever registered a command in this Stack state: each has
/// a lock file, which is never removed.
fn identities(state: &Path) -> Vec<String> {
    fs::read_dir(dir(state))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lock") && fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .filter_map(|p| Some(p.file_stem()?.to_str()?.to_string()))
        .collect()
}

/// Every readable registry in this Stack state: its identity and entries, live or not.
fn registries(state: &Path) -> Vec<(String, Vec<Entry>)> {
    fs::read_dir(dir(state))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_string(), read(&p).ok()?)))
        .collect()
}

/// Change the registry at `path` under `lock`: `change` gets the entries that still count
/// (live or unrecorded; those that are over are dropped).
fn update(path: &Path, lock: &Path, change: impl FnOnce(&mut Vec<Entry>)) -> Result<()> {
    let _lock = FileLock::acquire(lock)?;
    let mut entries = read(path)?;
    entries.retain(|e| !matches!(e.state(), State::Over));
    change(&mut entries);
    // Removed when empty: the lock file beside it stays (a lock file is never removed), and
    // keeps the checkout's command mark recognised (see [`Exempt`]).
    if entries.is_empty() {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(path.display(), e)),
            _ => Ok(()),
        }
    } else {
        write_json_unsynced(path, &entries)
    }
}

/// `down --forget-unrecorded`: remove the checkout's unrecorded entries (see
/// [`State::Unrecorded`]), under the registry lock, leaving every other entry. Returns how many.
pub fn forget_unrecorded(state: &Path, root: &Path) -> Result<usize> {
    let (path, lock) = files(state, root)?;
    let mut removed = 0;
    update(&path, &lock, |entries| {
        let before = entries.len();
        entries.retain(|e| !matches!(e.state(), State::Unrecorded));
        removed = before - entries.len();
    })?;
    Ok(removed)
}

static MCP: AtomicBool = AtomicBool::new(false);

/// This process is the MCP server: what it starts is registered as started through MCP.
pub fn serving_mcp() {
    MCP.store(true, Ordering::Relaxed);
}

/// How a command is started, as its entry names it.
pub enum StartedBy<'a> {
    Exec,
    Run(&'a str),
}

impl StartedBy<'_> {
    fn label(&self) -> String {
        let mcp = MCP.load(Ordering::Relaxed);
        match (self, mcp) {
            (StartedBy::Exec, false) => "exec".into(),
            (StartedBy::Exec, true) => "mcp stack_exec".into(),
            (StartedBy::Run(task), false) => format!("run {task}"),
            (StartedBy::Run(task), true) => format!("mcp stack_run {task}"),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Fail to start the thread that records a spawned command's PID, as a thread limit would.
    pub(crate) static WRITER_THREAD_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Fail the write that records a spawned command's PID, as a full disk would.
    pub(crate) static SPAWN_WRITE_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Where one registered command's entry is.
#[derive(Debug, Clone)]
struct Handle {
    path: PathBuf,
    lock: PathBuf,
    id: String,
}

impl Handle {
    #[cfg(test)]
    fn spawned(&self, pid: u32) -> Result<()> {
        self.record(procinfo::process(pid))
    }

    /// Record `process` (observed while it was a child not yet reaped) as the command.
    fn record(&self, process: Option<Process>) -> Result<()> {
        #[cfg(test)]
        if SPAWN_WRITE_FAILS.with(|f| f.get()) {
            return Err(StackError::new("io", "No space left on device (injected)"));
        }
        let Some(process) = process else { return Ok(()) };
        update(&self.path, &self.lock, |entries| {
            if let Some(e) = entries.iter_mut().find(|e| e.id == self.id) {
                e.command = Some(Observed::of(&process));
            }
        })
    }
}

/// A command's registration: removed when dropped (or by [`Registration::finish`]).
#[derive(Debug)]
pub struct Registration {
    handle: Handle,
    finished: bool,
    /// The write recording the spawned command, done beside the command's start; finished
    /// before the entry is removed.
    recording: Recording,
}

/// Register a command about to be started for the checkout at `root`, running `program`, by
/// this process. Called while it plans, under the project lock. An error means it is not
/// registered, and must not run.
pub fn register(state: &Path, root: &Path, started_by: StartedBy, program: &Path) -> Result<Registration> {
    let (path, lock) = files(state, root)?;
    let me = procinfo::process(std::process::id())
        .ok_or_else(|| StackError::new("state_invalid", "cannot observe this Stack process to register its command"))?;
    let id = crate::hash::sha256_hex(format!("{:?}{}{}", std::time::Instant::now(), me.pid, me.started).as_bytes())[..32].to_string();
    let entry = Entry {
        id: id.clone(),
        started_by: started_by.label(),
        program: program.file_name().map_or_else(|| program.display().to_string(), |n| n.to_string_lossy().into_owned()),
        coordinator: Observed::of(&me),
        command: None,
        registered_at_ms: now_ms(),
    };
    update(&path, &lock, |entries| entries.push(entry))?;
    Ok(Registration { handle: Handle { path, lock, id }, finished: false, recording: Default::default() })
}

/// Start the thread that records a command's process. Tests can make it fail as a thread
/// limit would.
fn writer_thread(job: impl FnOnce() + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if WRITER_THREAD_FAILS.with(|f| f.get()) {
        return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
    }
    std::thread::Builder::new().name("stack-command-record".into()).spawn(job)
}

type Recording = std::sync::Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>;

/// Record the process `pid` as the command of `handle`'s entry: observed now, while the child
/// cannot have been reaped, and written beside its start.
fn record_spawn(handle: &Handle, recording: &Recording, pid: u32) {
    // Observed now, while the child cannot have been reaped; written beside its start.
    let process = procinfo::process(pid);
    let record = |handle: Handle, process| {
        if let Err(e) = handle.record(process) {
            eprintln!("stack: cannot record the command's process in the command registry ({e}); once this Stack process is gone, `stack down` is not confirmed until `stack down --forget-unrecorded`");
        }
    };
    // Beside the command, so it does not wait for the write. A thread that cannot be
    // started (a process or thread limit) must not lose the record or end this process
    // with the command running: the write is then made here.
    let (theirs, observed) = (handle.clone(), process.clone());
    match writer_thread(move || record(theirs, observed)) {
        Ok(write) => {
            if let Ok(mut slot) = recording.lock() {
                *slot = Some(write);
            }
        }
        Err(_) => record(handle.clone(), process),
    }
}

impl Registration {
    /// Have the next process this thread spawns recorded as the command (see
    /// [`crate::process::on_next_spawn`]). If that write fails, the entry keeps counting: while
    /// this process lives, and after it, as unrecorded (see [`State::Unrecorded`]).
    pub fn record_next_spawn(&self) -> crate::process::SpawnHook {
        let handle = self.handle.clone();
        let recording = self.recording.clone();
        crate::process::on_next_spawn(Box::new(move |pid| record_spawn(&handle, &recording, pid)))
    }

    /// Remove the entry: the command ended.
    pub fn finish(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.finished, true) {
            return Ok(());
        }
        if let Some(write) = self.recording.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = write.join();
        }
        let id = self.handle.id.clone();
        update(&self.handle.path, &self.handle.lock, |entries| entries.retain(|e| e.id != id))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            eprintln!("stack: cannot remove this command from the command registry ({e}); it no longer counts once its process is gone");
        }
    }
}

/// A command (or a process holding this checkout's token) still running, as `status` and `down`
/// report it.
#[derive(Debug, Clone, Serialize)]
pub struct RunningCommand {
    /// Always `command`, so a `stop_unconfirmed` detail for a command is told from one for a
    /// service's process.
    pub kind: &'static str,
    /// The command's process; none while a registered command is being started.
    pub pid: Option<u32>,
    /// The program's name (never its arguments).
    pub program: String,
    /// How it was started: `exec`, `run <task>`, `mcp stack_exec`, `mcp stack_run <task>`,
    /// `older stack`; for a process found by its environment alone, what that shows.
    pub started_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_secs: Option<u64>,
    /// `registry`, `session` (an older Stack's), `mark` (its environment carries the checkout's
    /// command mark) or `token` (it carries the checkout's service token and is a command).
    pub found_by: &'static str,
    /// It belongs to another checkout (a command registered there, or the Stack process running
    /// one) and holds this checkout's service token: look at that checkout; never stop it from
    /// here.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub another_checkout: bool,
    /// Named in the hint's PIDs to wait for or stop: never a Stack process running commands (it
    /// may be an MCP server) nor another checkout's process.
    #[serde(skip)]
    in_hint: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn age(p: &Process) -> Option<u64> {
    p.started_unix_ms().map(|s| now_ms().saturating_sub(s) / 1000)
}

fn from_entry(e: &Entry, live: &Process) -> RunningCommand {
    let spawned = e.command.is_some();
    RunningCommand {
        kind: "command",
        pid: spawned.then_some(live.pid),
        program: e.program.clone(),
        started_by: e.started_by.clone(),
        running_secs: if spawned { age(live) } else { Some(now_ms().saturating_sub(e.registered_at_ms) / 1000) },
        found_by: "registry",
        another_checkout: false,
        in_hint: spawned,
    }
}

fn from_process(p: &Process, started_by: &str, found_by: &'static str) -> RunningCommand {
    let program = procinfo::named(p.pid).map(|n| n.comm).unwrap_or_else(|| "?".into());
    RunningCommand { kind: "command", pid: Some(p.pid), program, started_by: started_by.into(), running_secs: age(p), found_by, another_checkout: false, in_hint: true }
}

/// An unrecorded entry (see [`State::Unrecorded`]), as `down` and `status` describe it.
fn unrecorded(e: &Entry, path: &Path) -> String {
    let secs = now_ms().saturating_sub(e.registered_at_ms) / 1000;
    format!(
        "a command ({}, `{}`, registered {secs}s ago) whose process was never recorded in {}, and the Stack process that started it is gone",
        e.started_by,
        e.program,
        path.display()
    )
}

/// The registered commands of the checkout at `root` running now, as `status` lists them: the
/// registry alone, no process's environment read; with what makes the list incomplete, if
/// anything (an unrecorded entry).
pub fn listed(state: &Path, root: &Path) -> Result<(Vec<RunningCommand>, Option<String>)> {
    let (path, _) = files(state, root)?;
    let entries = read(&path)?;
    let unknown = entries.iter().find(|e| matches!(e.state(), State::Unrecorded)).map(|e| format!("{}; run `stack down --forget-unrecorded` once it no longer runs", unrecorded(e, &path)));
    Ok((entries.iter().filter_map(|e| e.live().map(|p| from_entry(e, &p))).collect(), unknown))
}

/// Most searches of every process's environment for the mark before `down` settles for what it
/// found.
const MARK_PASSES: usize = 5;

/// The processes carrying `mark`, searched until two passes agree (at most [`MARK_PASSES`]), with
/// those seen in any pass that still run.
fn marked(mark: &str) -> Vec<Process> {
    let mut seen: Vec<Process> = Vec::new();
    let mut last: Option<Vec<u32>> = None;
    for _ in 0..MARK_PASSES {
        let found = procinfo::carrying(crate::checkout::COMMAND_VAR, mark);
        let mut pids: Vec<u32> = found.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        for p in found {
            if !seen.iter().any(|q| q.same(&p)) {
                seen.push(p);
            }
        }
        if last.as_ref() == Some(&pids) {
            break;
        }
        last = Some(pids);
    }
    seen.retain(|p| p.current().is_some());
    seen
}

/// What `down` finds running for the checkout at `root` once its services are stopped:
///
/// - every live registered command, except the `down` process itself: when it is the command
///   (`stack exec -- stack down`), or the direct child of the coordinator of a command not yet
///   recorded (the same, before the spawn was recorded). A shell that runs `down` and carries
///   on counts;
/// - every process carrying the checkout's command mark;
/// - every process carrying its service token that `down` did not stop because it is a command
///   (see [`Exempt`]): never left out, but another checkout's is named as such, and a Stack
///   process running commands is not named in the hint;
/// - each Stack process an older Stack recorded in the session as running a command
///   (`older`, by PID; see [`older`]).
///
/// The registry is read under its lock; one that cannot be read, or that holds an unrecorded
/// entry, is an error.
pub fn after_down(state: &Path, root: &Path, older_pids: &[u32], exempt: &Exempt) -> Result<Vec<RunningCommand>> {
    // A registry this `down` could not read when it decided what to stop (so it stopped no
    // leftover) is not confirmed by this `down`, even if it has become readable since.
    if let Some(e) = &exempt.blind {
        let again = StackError::new("state_invalid", e.message.clone());
        return Err(match &e.hint {
            Some(hint) => again.hint(hint.clone()),
            None => again,
        });
    }
    let (path, lock) = files(state, root)?;
    let me = procinfo::process(std::process::id());
    let _lock = FileLock::acquire(&lock)?;
    let entries = read(&path)?;
    if let Some(e) = entries.iter().find(|e| matches!(e.state(), State::Unrecorded)) {
        return Err(StackError::new("state_invalid", unrecorded(e, &path))
            .hint("check that this command no longer runs (`ps`), then run `stack down --forget-unrecorded`, which removes only such entries"));
    }
    let mut found: Vec<RunningCommand> = Vec::new();
    let mut pids: Vec<u32> = me.iter().map(|m| m.pid).collect();
    for e in &entries {
        let Some(live) = e.live() else { continue };
        let itself = me.as_ref().is_some_and(|m| match &e.command {
            Some(c) => c.pid == m.pid,
            None => m.parent == e.coordinator.pid,
        });
        if itself {
            continue;
        }
        if e.command.is_some() {
            pids.push(live.pid);
        }
        found.push(from_entry(e, &live));
    }
    if let Some(mark) = crate::checkout::command_mark(root) {
        for p in marked(&mark) {
            if !pids.contains(&p.pid) && !exempt.coordinator(&p) {
                pids.push(p.pid);
                found.push(from_process(&p, "started by a command (found by its mark)", "mark"));
            }
        }
    }
    let identity = crate::checkout::identity(root);
    if let Some(token) = crate::checkout::token(root) {
        for p in procinfo::carrying(crate::checkout::TOKEN_VAR, &token) {
            if pids.contains(&p.pid) || !exempt.covers(&p) {
                continue;
            }
            pids.push(p.pid);
            let another = exempt.registered_elsewhere(&p, identity.as_deref());
            let coordinator = exempt.coordinator(&p);
            let what = match (another, coordinator) {
                (true, _) => "a process of another checkout holding this checkout's service token",
                (false, true) => "a Stack process running a command, holding this checkout's service token",
                (false, false) => "a command a service of this checkout started",
            };
            let mut c = from_process(&p, what, "token");
            c.another_checkout = another;
            c.in_hint = !another && !coordinator;
            found.push(c);
        }
    }
    found.extend(older(older_pids, &pids, exempt));
    found.sort_by_key(|c| c.pid);
    Ok(found)
}

/// Each Stack process an older Stack recorded in the session as running a command
/// (`active_executions`, by PID alone) that still runs and that no registry names as a
/// coordinator (a current Stack registers its own). Only its PID is known: nothing it started
/// is looked for.
fn older(recorded: &[u32], known: &[u32], exempt: &Exempt) -> Vec<RunningCommand> {
    recorded
        .iter()
        .filter(|pid| !known.contains(pid))
        .filter_map(|pid| procinfo::process(*pid))
        .filter(|p| !exempt.coordinator(p))
        .map(|p| {
            // Possibly an older MCP server: never named as something to stop.
            let mut c = from_process(&p, "older stack", "session");
            c.in_hint = false;
            c
        })
        .collect()
}

/// `Ok` when nothing runs; otherwise `stop_unconfirmed` for a `down` that stopped `stopped`
/// services, listing the commands, or saying why they cannot be known.
pub fn confirm(stopped: usize, running: Result<Vec<RunningCommand>>) -> Result<()> {
    let commands = match running {
        Ok(commands) if commands.is_empty() => return Ok(()),
        Ok(commands) => commands,
        Err(e) => {
            return Err(StackError::new(
                "stop_unconfirmed",
                format!("stopped {stopped} service(s), but cannot tell whether a command Stack started for this checkout still runs: {}", e.message),
            )
            .hint(e.hint.clone().unwrap_or_else(|| "fix the command registry the message names, then run `stack down` again".into()))
            .details(vec![json!({ "kind": "registry", "error": e.message })]));
        }
    };
    let pids: Vec<String> = commands.iter().filter(|c| c.in_hint).filter_map(|c| c.pid).map(|p| p.to_string()).collect();
    let n = commands.len();
    let mut hint = format!("they still hold this checkout's environment (DATABASE_URL, ports) and would reach whatever listens there next; Stack never stops them: {}", if pids.is_empty() {
        "wait for them to finish".to_string()
    } else {
        format!("wait for them to finish or stop them (pid {})", pids.join(", "))
    });
    if commands.iter().any(|c| c.another_checkout) {
        hint.push_str("; a process of another checkout holds this checkout's service token: look at that checkout (`stack status` there) rather than stopping it from here");
    }
    if commands.iter().any(|c| c.found_by == "session") {
        hint.push_str("; a command an older Stack started is tracked only while its Stack process runs: wait for that call to finish, or restart that session under this Stack (stop it there and run the command again with this Stack)");
    }
    hint.push_str(", then run `stack down` again to confirm");
    Err(StackError::new(
        "stop_unconfirmed",
        format!("stopped {stopped} service(s), but {n} command(s) Stack started for this checkout {} still running", if n == 1 { "is" } else { "are" }),
    )
    .hint(hint)
    .details(commands.iter().map(|c| json!(c)).collect()))
}

/// The processes `down`, `up` and a supervisor's recovery must never signal, as the registries
/// prove it: every live registered command and coordinator of any checkout in this Stack state
/// (by PID, start time and boot), any process carrying the command mark of a checkout that ever
/// registered a command here (its lock file stays), and any process carrying the mark of the
/// checkout this is loaded for, which needs no registry. A mark of any other
/// value exempts nothing. Each decision is taken once and kept, so one `down` stops and reports
/// by the same answer.
#[derive(Debug, Default)]
pub struct Exempt {
    /// Live registered processes, with the identity of the registry naming them and whether
    /// they are coordinators.
    known: Vec<(Observed, String, bool)>,
    marks: Vec<(String, String)>,
    /// Why this checkout's own registry cannot be read: then nothing outside the services'
    /// recorded process groups may be signalled.
    pub blind: Option<StackError>,
    decided: std::cell::RefCell<Vec<(Process, Option<String>)>>,
}

impl Exempt {
    /// For the checkout at `root`, when known.
    pub fn load(state: &Path, root: Option<&Path>) -> Self {
        let (mut known, mut marks) = (Vec::new(), Vec::new());
        for identity in identities(state) {
            marks.push((crate::checkout::command_mark_of(&identity), identity));
        }
        for (identity, entries) in registries(state) {
            for e in entries {
                known.push((e.coordinator, identity.clone(), true));
                if let Some(c) = e.command {
                    known.push((c, identity.clone(), false));
                }
            }
        }
        let mut blind = None;
        if let Some(root) = root {
            if let Some(identity) = crate::checkout::identity(root) {
                marks.push((crate::checkout::command_mark_of(&identity), identity));
            }
            blind = files(state, root).and_then(|(path, _)| read(&path)).err();
        }
        Self { known, marks, blind, decided: Default::default() }
    }

    /// Whether `p` is a Stack process running a registered command (an `exec`, a `run`, an MCP
    /// server).
    fn coordinator(&self, p: &Process) -> bool {
        self.known.iter().any(|(o, _, coordinating)| *coordinating && o.is(p))
    }

    /// The identity of the checkout `p` is registered to (as a command or coordinator), or whose
    /// command mark it carries: decided once per process.
    fn checkout_of(&self, p: &Process) -> Option<String> {
        if let Some((_, decided)) = self.decided.borrow().iter().find(|(q, _)| q.same(p)) {
            return decided.clone();
        }
        let decided = self.known.iter().find(|(o, _, _)| o.is(p)).map(|(_, identity, _)| identity.clone()).or_else(|| {
            let mark = procinfo::environ_value(p.pid, crate::checkout::COMMAND_VAR).flatten()?;
            self.marks.iter().find(|(m, _)| *m == mark).map(|(_, identity)| identity.clone())
        });
        self.decided.borrow_mut().push((p.clone(), decided.clone()));
        decided
    }

    /// Whether `p` belongs to a checkout other than `identity`.
    fn registered_elsewhere(&self, p: &Process, identity: Option<&str>) -> bool {
        self.checkout_of(p).is_some_and(|other| Some(other.as_str()) != identity)
    }

    pub fn covers(&self, p: &Process) -> bool {
        self.checkout_of(p).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(coordinator: Observed, command: Option<Observed>) -> Entry {
        Entry { id: "e".into(), started_by: "exec".into(), program: "p".into(), coordinator, command, registered_at_ms: 0 }
    }

    fn me() -> Observed {
        Observed::of(&procinfo::process(std::process::id()).unwrap())
    }

    /// A process of this test binary sleeping (its environment is readable), with `env`.
    fn child(env: &[(&str, &str)]) -> std::process::Child {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "commands::tests::sleeper", "--ignored", "--test-threads", "1"]).envs(env.iter().copied());
        command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap()
    }

    #[test]
    #[ignore = "a long-running child for the other tests"]
    fn sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    /// A process that exited: its observation names nothing that runs.
    fn gone() -> Observed {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let observed = Observed { pid: c.id(), started: 1, boot: procinfo::boot_id() };
        c.wait().unwrap();
        observed
    }

    /// An entry counts only while its process is the one observed: a PID now held by another
    /// process (another start time), or recorded in another boot, never does.
    #[test]
    fn an_entry_is_live_only_while_its_process_is_the_one_observed() {
        let me = me();
        let reused = Observed { started: me.started + 1, ..me.clone() };
        let rebooted = Observed { boot: Some("another boot".into()), ..me.clone() };
        assert!(entry(me.clone(), None).live().is_some(), "before the spawn, its coordinator");
        assert!(entry(me.clone(), Some(me.clone())).live().is_some());
        assert!(entry(me.clone(), Some(reused.clone())).live().is_none(), "the command's PID, reused");
        assert!(entry(me.clone(), Some(rebooted)).live().is_none(), "another boot's record");
        assert!(matches!(entry(reused, Some(me.clone())).state(), State::Live(_)), "the command counts after its coordinator");
    }

    /// A registration is listed while it lives and removed when dropped; an unreadable registry
    /// is an error, never an empty list.
    #[test]
    fn a_registration_is_listed_until_dropped_and_an_unreadable_registry_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("root"));
        fs::create_dir_all(&root).unwrap();
        let registration = register(&state, &root, StartedBy::Run("t"), Path::new("/bin/prog")).unwrap();
        let (listed, unknown) = listed(&state, &root).unwrap();
        assert_eq!((listed.len(), listed[0].pid, listed[0].started_by.as_str(), listed[0].program.as_str(), unknown), (1, None, "run t", "prog", None));
        registration.handle.spawned(std::process::id()).unwrap();
        assert_eq!(super::listed(&state, &root).unwrap().0[0].pid, Some(std::process::id()));
        drop(registration);
        assert!(super::listed(&state, &root).unwrap().0.is_empty());
        let (path, lock) = files(&state, &root).unwrap();
        assert!(!path.exists() && lock.exists(), "the lock file stays, so the checkout's mark stays known");
        let mark = crate::checkout::command_mark(&root).unwrap();
        assert!(Exempt::load(&state, None).marks.iter().any(|(m, _)| *m == mark));
        fs::write(&path, "nonsense").unwrap();
        assert!(super::listed(&state, &root).is_err());
        assert!(after_down(&state, &root, &[], &Exempt::default()).is_err());
    }

    /// A `down` that could not read the registry when it decided what to stop is not confirmed,
    /// even if the registry has become readable (or was cleared) since.
    #[test]
    fn a_down_that_began_with_an_unreadable_registry_is_never_confirmed() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("root"));
        fs::create_dir_all(&root).unwrap();
        let blind = Exempt { blind: Some(StackError::new("state_invalid", "the command registry cannot be read")), ..Exempt::default() };
        assert!(after_down(&state, &root, &[], &Exempt::default()).unwrap().is_empty(), "nothing registered, and readable now");
        let error = confirm(2, after_down(&state, &root, &[], &blind)).unwrap_err();
        assert_eq!((error.code, error.details[0]["kind"].as_str()), ("stop_unconfirmed", Some("registry")), "{}", error.message);
    }

    /// The thread that records a spawned command's PID cannot be started (a thread limit): the
    /// PID is recorded all the same, by the caller, so the command stays known.
    #[test]
    fn a_writer_thread_that_cannot_start_does_not_lose_the_commands_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("root"));
        fs::create_dir_all(&root).unwrap();
        let mut registration = register(&state, &root, StartedBy::Exec, Path::new("/bin/sleep")).unwrap();
        let mut command = child(&[]);
        WRITER_THREAD_FAILS.with(|f| f.set(true));
        record_spawn(&registration.handle, &registration.recording, command.id());
        WRITER_THREAD_FAILS.with(|f| f.set(false));
        assert!(registration.recording.lock().unwrap().is_none(), "no thread was started");
        let running = after_down(&state, &root, &[], &Exempt::default()).unwrap();
        assert_eq!((running.len(), running[0].pid), (1, Some(command.id())), "recorded by its own PID");
        command.kill().unwrap();
        command.wait().unwrap();
        registration.finish().unwrap();
    }

    /// The write recording a spawned command's PID fails (a full disk): the entry stays, and once
    /// its coordinator is gone it is unrecorded, which `down` reports as an error (never as
    /// nothing running) and no later write drops; `status` still lists the live entries beside
    /// it. `--forget-unrecorded` removes it and nothing else.
    #[test]
    fn a_command_whose_pid_was_never_recorded_keeps_down_unconfirmed_until_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("root"));
        fs::create_dir_all(&root).unwrap();
        let registration = register(&state, &root, StartedBy::Exec, Path::new("/bin/sleep")).unwrap();
        SPAWN_WRITE_FAILS.with(|f| f.set(true));
        let failed = registration.handle.spawned(std::process::id());
        SPAWN_WRITE_FAILS.with(|f| f.set(false));
        assert!(failed.is_err());
        let (path, lock) = files(&state, &root).unwrap();
        // Its coordinator then dies without removing it.
        std::mem::forget(registration);
        update(&path, &lock, |entries| entries[0].coordinator = gone()).unwrap();
        let none = Exempt::default();
        let error = after_down(&state, &root, &[], &none).unwrap_err();
        assert!(error.message.contains("never recorded") && error.message.contains("exec"), "{}", error.message);
        assert!(error.hint.as_deref().unwrap().contains("--forget-unrecorded") && !error.hint.as_deref().unwrap().contains("delet"), "{:?}", error.hint);
        assert!(confirm(0, after_down(&state, &root, &[], &none)).unwrap_err().details[0]["kind"] == "registry");
        // Another command registered meanwhile: listed beside the unknown one, and kept.
        let mut live = register(&state, &root, StartedBy::Run("t"), Path::new("/bin/true")).unwrap();
        let (listed, unknown) = listed(&state, &root).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(unknown.unwrap().contains("--forget-unrecorded"));
        assert!(after_down(&state, &root, &[], &none).is_err());
        assert_eq!(forget_unrecorded(&state, &root).unwrap(), 1);
        assert!(super::listed(&state, &root).unwrap().1.is_none());
        let running = after_down(&state, &root, &[], &none).unwrap();
        assert_eq!((running.len(), running[0].pid), (1, None), "the other command, being started, is kept");
        live.finish().unwrap();
    }

    /// Only a registry exempts: a process carrying a mark no registry here is for is not exempt,
    /// one carrying the mark of a registered checkout is.
    #[test]
    fn an_arbitrary_mark_exempts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, root) = (tmp.path().join("state"), tmp.path().join("root"));
        fs::create_dir_all(&root).unwrap();
        let _registration = register(&state, &root, StartedBy::Exec, Path::new("/bin/prog")).unwrap();
        let mark = crate::checkout::command_mark(&root).unwrap();
        let mut stranger = child(&[(crate::checkout::COMMAND_VAR, "0123456789abcdef0123456789abcdef")]);
        let mut marked = child(&[(crate::checkout::COMMAND_VAR, &mark)]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while procinfo::environ_value(marked.id(), crate::checkout::COMMAND_VAR).flatten().as_deref() != Some(mark.as_str()) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        while procinfo::environ_value(stranger.id(), crate::checkout::COMMAND_VAR).flatten().is_none() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let exempt = Exempt::load(&state, None);
        let covered = |c: &std::process::Child| exempt.covers(&procinfo::process(c.id()).unwrap());
        let (s, m) = (covered(&stranger), covered(&marked));
        for c in [&mut stranger, &mut marked] {
            c.kill().unwrap();
            c.wait().unwrap();
        }
        assert!(!s, "an arbitrary mark exempted a process");
        assert!(m, "the mark of a registered checkout");
        assert!(exempt.covers(&procinfo::process(std::process::id()).unwrap()), "a registered coordinator");
    }
}
