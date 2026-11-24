#![cfg(unix)]
//! `down` and `status` with commands Stack started for the checkout still running (`exec`, `run`,
//! MCP calls), against the fake mise of tests/fakes: `down` stops the services but is not
//! confirmed while one runs, and never signals it; `status` lists the registered ones. The
//! commands these tests run are this test binary's `holder` (below), whose environment macOS
//! lets its owner read. Real services (stopped, data kept by `--purge`) are covered in
//! tests/worktree_isolation.rs.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

/// An argument every held command gets, standing for a secret: no output may show it.
const SECRET_ARG: &str = "hunter2-SECRET-ARG";

struct Fixture {
    dir: TempDir,
}

/// The Stack under test: `STACK_TEST_BIN` runs another build (to show a test fails without its fix).
fn bin() -> String {
    std::env::var("STACK_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_stack").to_string())
}

impl Fixture {
    fn new() -> Self {
        Self::with_bundle("[bundle]\nname='test'\n")
    }

    fn with_bundle(bundle: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bin", "bundle", "app"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("bundle/bundle.toml"), bundle).unwrap();
        fs::write(dir.path().join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
        fs::write(dir.path().join("daemons.json"), "[]").unwrap();
        let path = format!("{}:{}", dir.path().join("bin").display(), std::env::var("PATH").unwrap());
        fs::write(dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
        let mise = dir.path().join("bin/mise");
        fs::write(&mise, FAKE_MISE).unwrap();
        fs::set_permissions(mise, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self { dir };
        fixture.ok(&["compile"]);
        fixture
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Another checkout using the same bundle, compiled.
    fn add_project(&self, name: &str) -> PathBuf {
        let dir = self.path(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
        self.ok_at(&dir, &["compile"]);
        dir
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_at(&self.path("app"), args)
    }

    fn command_at(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(bin());
        command
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("PATH", format!("{}:{}", self.path("bin").display(), std::env::var("PATH").unwrap()))
            .env("REVIEW_FIXTURE", self.dir.path())
            .env("REVIEW_PYTHON", python::python())
            .env("STACK_STATE_DIR", self.path("state"))
            .env("STACK_CACHE_DIR", self.path("cache"))
            .env("PITCHFORK_STATE_DIR", self.path("pfs"))
            .env_remove("STACK_COMMAND_MARK")
            .stdin(Stdio::null());
        command
    }

    fn ok(&self, args: &[&str]) -> Output {
        self.ok_at(&self.path("app"), args)
    }

    fn ok_at(&self, dir: &Path, args: &[&str]) -> Output {
        let out = self.command_at(dir, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    /// `--json <args>` at `dir`: the exit code and the result.
    fn json_at(&self, dir: &Path, args: &[&str]) -> (Option<i32>, Value) {
        let out = self.command_at(dir, &[&["--json"], args].concat()).output().unwrap();
        let value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (out.status.code(), value)
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        self.json_at(&self.path("app"), args)
    }

    /// `stack exec -- holder` at `dir`, in the background: `mode` as [`holder`] reads it, the
    /// process(es) it starts writing their PID to `pidfile`.
    fn hold_at(&self, dir: &Path, mode: &str, secs: u64, pidfile: &Path) -> Child {
        let holder = std::env::current_exe().unwrap();
        // The last argument stands for a secret given on the command line: never reported.
        self.command_at(dir, &["exec", "--", holder.to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1", SECRET_ARG])
            .env("HOLD_MODE", mode)
            .env("HOLD_SECS", secs.to_string())
            .env("HOLD_PIDFILE", pidfile)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn hold(&self, mode: &str, secs: u64, pidfile: &Path) -> Child {
        self.hold_at(&self.path("app"), mode, secs, pidfile)
    }
}

/// Set by the first SIGTERM a `listen` holder with `HOLD_WORKER` gets.
static TERMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERMED.store(true, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn leave_group(_: libc::c_int) {
    // SAFETY: setpgid is async-signal-safe.
    unsafe { libc::setpgid(0, 0) };
}

/// Block (or unblock) SIGTERM for the calling thread; async-signal-safe.
fn block_term(how: libc::c_int) -> std::io::Result<()> {
    // SAFETY: a signal set on the stack, filled and passed to sigprocmask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        if libc::sigprocmask(how, &set, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The command these tests run through Stack. `HOLD_MODE`: `sleep` (default) writes its PID to
/// `HOLD_PIDFILE` and sleeps `HOLD_SECS`; `group` and `session` start a copy of itself that does
/// that, in a process group or a session of its own, and exit at once, as a command that
/// daemonizes does. `listen` holds `HOLD_PORT` too; with `HOLD_WORKER` (a mode) it starts, on its
/// first SIGTERM, a worker of that mode in its own process group, without the checkout's token
/// or mark, writing its PID to `HOLD_WORKER_PIDFILE`, then exits. `leave` (a worker mode) sleeps
/// and leaves its process group on its first SIGTERM, staying alive. `group` and `session` start
/// their copy in `HOLD_CHILD_MODE` (default `sleep`); `exec` writes its PID, waits for the file
/// `HOLD_TRIGGER`, then replaces itself (same PID) with `/bin/sleep`, whose environment macOS
/// hides.
#[test]
#[ignore = "a command for the other tests to run through `stack exec`"]
fn holder() {
    use std::os::unix::process::CommandExt;
    let mode = std::env::var("HOLD_MODE").unwrap_or_default();
    if mode == "leave" {
        // SAFETY: installs a handler that only calls setpgid.
        unsafe { libc::signal(libc::SIGTERM, leave_group as *const () as libc::sighandler_t) };
    }
    if mode == "group" || mode == "session" {
        let child_mode = std::env::var("HOLD_CHILD_MODE").unwrap_or_else(|_| "sleep".into());
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "holder", "--ignored", "--test-threads", "1"]).env("HOLD_MODE", child_mode).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if mode == "group" {
            child.process_group(0);
        } else {
            // SAFETY: setsid is async-signal-safe, as pre_exec requires.
            unsafe {
                child.pre_exec(|| if libc::setsid() < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
            }
        }
        // Left running on purpose: it outlives this process, as a daemonized child does.
        #[allow(clippy::zombie_processes)]
        child.spawn().unwrap();
        return;
    }
    // `listen`: hold `HOLD_PORT` open, as a service would.
    let _listener = (mode == "listen").then(|| std::net::TcpListener::bind(("127.0.0.1", std::env::var("HOLD_PORT").unwrap().parse::<u16>().unwrap())).unwrap());
    let worker = std::env::var("HOLD_WORKER").ok().filter(|_| mode == "listen");
    if worker.is_some() {
        // SAFETY: installs a handler that only stores to an atomic.
        unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };
    }
    let pidfile = std::env::var("HOLD_PIDFILE").unwrap();
    fs::write(format!("{pidfile}.tmp"), std::process::id().to_string()).unwrap();
    fs::rename(format!("{pidfile}.tmp"), &pidfile).unwrap();
    // A worker started with SIGTERM blocked gets one sent meanwhile only now, with its handler set
    // and its PID published.
    block_term(libc::SIG_UNBLOCK).unwrap();
    if mode == "exec" {
        let trigger = std::env::var("HOLD_TRIGGER").unwrap();
        while !Path::new(&trigger).exists() {
            thread::sleep(Duration::from_millis(10));
        }
        let error = Command::new("/bin/sleep").arg("60").exec();
        panic!("exec /bin/sleep: {error}");
    }
    let secs = std::env::var("HOLD_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let Some(worker) = worker else {
        thread::sleep(Duration::from_secs(secs));
        return;
    };
    let until = Instant::now() + Duration::from_secs(secs);
    while !TERMED.load(std::sync::atomic::Ordering::SeqCst) {
        if Instant::now() >= until {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let worker_file = std::env::var("HOLD_WORKER_PIDFILE").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "holder", "--ignored", "--test-threads", "1"])
        .env("HOLD_MODE", worker)
        .env("HOLD_SECS", "60")
        .env("HOLD_PIDFILE", &worker_file)
        .env_remove(stack::checkout::TOKEN_VAR)
        .env_remove(stack::checkout::COMMAND_VAR)
        .process_group(std::process::id() as i32)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: sigprocmask is async-signal-safe. Blocked, a SIGTERM sent before the worker set
    // its handler waits for it.
    unsafe { child.pre_exec(|| block_term(libc::SIG_BLOCK)) };
    // Left running on purpose: it outlives this process.
    #[allow(clippy::zombie_processes)]
    child.spawn().unwrap();
    pid_in(Path::new(&worker_file));
}

/// The PID written to `path`, waiting up to 10 seconds for it.
fn pid_in(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = fs::read_to_string(path).ok().and_then(|t| t.trim().parse().ok()) {
            return pid;
        }
        assert!(Instant::now() < deadline, "{} never written", path.display());
        thread::sleep(Duration::from_millis(10));
    }
}

fn alive(pid: u32) -> bool {
    stack::procinfo::process(pid).is_some()
}

/// Kill `pid` and wait (up to 5 seconds) until it is gone; it is not this test's child.
fn end(pid: u32) {
    // SAFETY: kill only sends a signal, to a process this test started through Stack.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(Instant::now() < deadline, "{pid} did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Kills what it holds when dropped, so a failed assertion leaves nothing running.
#[derive(Default)]
struct Reaper {
    /// Processes that are not this test's children, as observed (PID and start time): only
    /// the very process observed is ever signalled, never a later one given its PID.
    pids: std::cell::RefCell<Vec<stack::procinfo::Process>>,
    children: std::cell::RefCell<Vec<Child>>,
}

impl Reaper {
    fn pid(&self, pid: u32) -> u32 {
        if let Some(p) = stack::procinfo::process(pid) {
            self.pids.borrow_mut().push(p);
        }
        pid
    }

    /// Hold `child`; its PID.
    fn child(&self, child: Child) -> u32 {
        let pid = child.id();
        self.children.borrow_mut().push(child);
        pid
    }

    /// Wait for the held child `pid` to exit; its exit code.
    fn wait(&self, pid: u32) -> Option<i32> {
        let mut children = self.children.borrow_mut();
        let i = children.iter().position(|c| c.id() == pid).expect("a held child");
        let mut child = children.remove(i);
        child.wait().unwrap().code()
    }

    fn running(&self, pid: u32) -> bool {
        self.children.borrow_mut().iter_mut().find(|c| c.id() == pid).map_or(alive(pid), |c| c.try_wait().unwrap().is_none())
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for p in self.pids.borrow().iter().filter(|p| p.current().is_some()) {
            // SAFETY: kill only sends a signal, to a process this test started, just checked to
            // be still that process.
            unsafe { libc::kill(p.pid as i32, libc::SIGKILL) };
        }
        for child in self.children.borrow_mut().iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The detail or entry of `commands` for `pid`.
fn naming(commands: &Value, pid: u32) -> Value {
    commands.as_array().unwrap().iter().find(|c| c["pid"] == pid).cloned().unwrap_or_else(|| panic!("no entry for {pid}: {commands}"))
}

fn assert_unconfirmed(code: Option<i32>, down: &Value) {
    assert_eq!(down["ok"], false, "{down}");
    assert_eq!(down["error"]["code"], "stop_unconfirmed", "{down}");
    assert_eq!(down["error"]["retryable"], true, "{down}");
    // The same exit as any other refusal to confirm.
    assert_eq!(code, Some(70), "{down}");
    assert!(!down.to_string().contains(SECRET_ARG), "arguments reported: {down}");
}

fn assert_confirmed(code: Option<i32>, down: &Value) {
    assert_eq!((code, &down["data"]["confirmed"]), (Some(0), &json!(true)), "{down}");
}

/// The registry file of the checkout at `dir`.
fn registry(fixture: &Fixture, dir: &Path) -> PathBuf {
    fixture.path("state/commands").join(format!("{}.json", stack::checkout::identity(dir).unwrap()))
}

/// A command run with `stack exec`: `status` lists it (its own PID, program, how it was started,
/// its age; never its arguments); `down` reports it as `stop_unconfirmed` with a `kind: command`
/// detail and a hint naming its PID, never signals it, and says the same on every later `down`
/// until it has exited, after which `down` confirms.
#[test]
fn down_is_not_confirmed_while_an_exec_command_runs_and_never_signals_it() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("sleep", 60, &pidfile));
    let holder = reaper.pid(pid_in(&pidfile));

    let (code, status) = fixture.json(&["status"]);
    assert_eq!(code, Some(0), "running commands do not make the checkout unhealthy: {status}");
    let listed = naming(&status["data"]["commands"], holder);
    assert_eq!(listed["kind"], "command", "{status}");
    assert_eq!(listed["started_by"], "exec", "{status}");
    assert_eq!(listed["found_by"], "registry", "{status}");
    let exe = std::env::current_exe().unwrap();
    assert_eq!(listed["program"], exe.file_name().unwrap().to_str().unwrap(), "{status}");
    assert!(listed["running_secs"].is_u64(), "{status}");
    assert!(!status.to_string().contains(SECRET_ARG), "{status}");
    let text = String::from_utf8_lossy(&fixture.ok(&["status"]).stdout).into_owned();
    assert!(text.contains(&format!("pid {holder}")) && !text.contains(SECRET_ARG), "{text}");

    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    let error = &down["error"];
    assert!(error["message"].as_str().unwrap().contains("1 command(s) Stack started for this checkout is still running"), "{down}");
    assert_eq!(naming(&error["details"], holder)["kind"], "command", "{down}");
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&format!("pid {holder}")) && !hint.contains(&exec.to_string()), "the command, not its Stack process: {hint}");
    assert!(!fixture.path("app/.stack/session.json").exists(), "the session was removed");

    let out = fixture.command(&["down"]).output().unwrap();
    assert_eq!(out.status.code(), Some(70));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("error[stop_unconfirmed]") && text.contains(&holder.to_string()) && !text.contains(SECRET_ARG), "{text}");

    let (_, status) = fixture.json(&["status"]);
    assert!(status["data"]["session"].is_null(), "{status}");
    naming(&status["data"]["commands"], holder);
    // The registered command makes it started; the unconfirmed `down` is said all the same.
    assert!(status["data"]["found"][0].as_str().unwrap_or_default().contains("the last `stack down` here"), "{status}");
    let text = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
    assert!(text.contains(&format!("pid {holder}")) && text.contains("found        the last `stack down` here"), "{text}");
    let (code, again) = fixture.json(&["down"]);
    assert_unconfirmed(code, &again);
    assert!(alive(holder) && reaper.running(exec), "down signalled the command");

    end(holder);
    reaper.wait(exec);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    assert_eq!(fixture.json(&["status"]).1["data"]["commands"], json!([]));
    let file = registry(&fixture, &fixture.path("app"));
    assert!(!file.exists() && file.with_extension("lock").exists(), "removed as commands end; its lock file stays");
}

/// In a checkout that never ran `up` (no session), a command is registered all the same: here
/// `/bin/sleep`, whose environment macOS hides, so only the registry can show it.
#[test]
fn a_command_in_a_checkout_without_a_session_keeps_down_purge_unconfirmed() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    let exec = reaper.child(fixture.command(&["exec", "--", "/bin/sleep", "60"]).stdout(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let sleep = loop {
        let (_, status) = fixture.json(&["status"]);
        if let Some(pid) = status["data"]["commands"][0]["pid"].as_u64() {
            break reaper.pid(pid as u32);
        }
        assert!(Instant::now() < deadline, "{status}");
        thread::sleep(Duration::from_millis(20));
    };
    assert_ne!(sleep, exec);
    let (code, down) = fixture.json(&["down", "--purge"]);
    assert_unconfirmed(code, &down);
    assert_eq!(naming(&down["error"]["details"], sleep)["program"], "sleep", "{down}");
    end(sleep);
    reaper.wait(exec);
    let (code, down) = fixture.json(&["down", "--purge"]);
    assert_confirmed(code, &down);
}

/// A checkout with no session is "not started" (`status` exits 0) only while nothing Stack
/// started for it runs: a registered command still running makes it `started`, and with a
/// service that is not verified that is unhealthy (exit 1), as before. Once the command ends,
/// it is not started again.
#[test]
fn a_command_running_without_a_session_is_not_reported_as_not_started() {
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let (code, status) = fixture.json(&["status"]);
    assert_eq!((code, &status["data"]["started"], &status["passed"]), (Some(0), &json!(false), &json!(true)), "{status}");
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("sleep", 60, &pidfile));
    let holder = reaper.pid(pid_in(&pidfile));

    // Registered before it was spawned, the command counts whether or not its PID is recorded
    // yet (until then it is listed as starting).
    let (code, status) = fixture.json(&["status"]);
    assert!(status["data"]["session"].is_null(), "{status}");
    assert_eq!(status["data"]["commands"].as_array().map(Vec::len), Some(1), "{status}");
    assert_eq!((code, &status["data"]["started"], &status["data"]["healthy"], &status["passed"]), (Some(1), &json!(true), &json!(false), &json!(false)), "{status}");
    let out = fixture.command(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(!text.contains("not started") && text.contains("NOT READY") && text.contains("command"), "{text}");
    // Once the registry records its PID, it is named by it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.json(&["status"]).1["data"]["commands"][0]["pid"] != holder {
        assert!(Instant::now() < deadline, "the registry never recorded pid {holder}");
        thread::sleep(Duration::from_millis(20));
    }

    end(holder);
    reaper.wait(exec);
    let out = fixture.command(&["status"]).output().unwrap();
    assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned()), (Some(0), "not started; run `stack up`\n".to_string()));
}

/// What a command started and left running when it exited (a child in a process group or a
/// session of its own) carries the checkout's command mark: `down` finds it by that.
#[test]
fn a_detached_child_in_its_own_group_or_session_keeps_down_unconfirmed() {
    for mode in ["group", "session"] {
        let reaper = Reaper::default();
        let fixture = Fixture::new();
        fixture.ok(&["up"]);
        let pidfile = fixture.path("holder.pid");
        let exec = reaper.child(fixture.hold(mode, 60, &pidfile));
        assert_eq!(reaper.wait(exec), Some(0), "{mode}");
        let child = reaper.pid(pid_in(&pidfile));
        assert_eq!(stack::procinfo::process(child).unwrap().group, child, "{mode}: the child leads a group of its own");

        let (code, down) = fixture.json(&["down"]);
        assert_unconfirmed(code, &down);
        assert_eq!(naming(&down["error"]["details"], child)["found_by"], "mark", "{mode}: {down}");
        assert!(alive(child), "{mode}: down signalled it");
        end(child);
        let (code, down) = fixture.json(&["down"]);
        assert_confirmed(code, &down);
    }
}

/// A helper a `stack exec` command left running in a group of its own, once that command ended:
/// nothing registered or supervised names it, but it carries the checkout's command mark. While
/// it runs, `down` is not confirmed and `status` is not "not started": it is `started`, exits 1
/// (a service is not verified) and names the helper; once it is gone, the checkout is not
/// started.
#[test]
fn status_is_not_not_started_while_a_detached_helper_of_a_command_runs() {
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("group", 60, &pidfile));
    assert_eq!(reaper.wait(exec), Some(0));
    let child = reaper.pid(pid_in(&pidfile));
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], child);

    let (code, status) = fixture.json(&["status"]);
    assert_eq!(status["data"]["commands"], json!([]), "{status}");
    assert_eq!((code, &status["data"]["started"], &status["passed"]), (Some(1), &json!(true), &json!(false)), "{status}");
    assert!(status["data"]["found"].to_string().contains(&format!("process {child} ")), "{status}");
    let out = fixture.command(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.starts_with("no session") && text.contains(&format!("found        process {child} ")), "{text}");

    end(child);
    // Gone, but no `down` has confirmed that yet.
    assert_marked_unconfirmed(&fixture);
    assert_confirmed_then_not_started(&fixture);
}

/// A `down` that is not confirmed and cannot record that (where its marker's directory belongs
/// is a regular file, which no privilege writes into) still fails, and says that `status` may
/// not show it.
#[test]
fn an_unconfirmed_down_that_cannot_be_recorded_says_so() {
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("group", 60, &pidfile));
    assert_eq!(reaper.wait(exec), Some(0));
    let child = reaper.pid(pid_in(&pidfile));
    let markers = fixture.path("state/unconfirmed");
    fs::create_dir_all(markers.parent().unwrap()).unwrap();
    fs::write(&markers, "not a directory").unwrap();
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], child);
    assert!(down["error"]["message"].as_str().unwrap().contains("could not be recorded, so `stack status` may not show it"), "{down}");
    assert!(down["error"]["details"].as_array().unwrap().iter().any(|d| d["kind"] == "marker" && d["error"].is_string()), "{down}");
    assert_eq!(fs::read_to_string(&markers).unwrap(), "not a directory", "nothing was written");
    fs::remove_file(&markers).unwrap();
    end(child);
    assert_confirmed_then_not_started(&fixture);
}

/// A `stack exec` started with the checkout's service token in its environment, as a service
/// that runs `stack exec` starts it: `down`'s search for leftover service processes never takes
/// it, or its command, for one. It is reported as a running command and never signalled.
#[test]
fn a_command_carrying_the_service_token_is_reported_and_never_signalled() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let pidfile = fixture.path("holder.pid");
    let exec = {
        let mut command = fixture.command(&["exec", "--", std::env::current_exe().unwrap().to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env(stack::checkout::TOKEN_VAR, &token).env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    let holder = reaper.pid(pid_in(&pidfile));
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], holder);
    assert!(reaper.running(exec) && alive(holder), "down signalled a command carrying the token");
    end(holder);
    assert_eq!(reaper.wait(exec), Some(137), "it ended by the test's own SIGKILL, passed on as 128 + 9");
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// `stack exec -- stack down` does not wait for itself; a shell that runs `stack down --purge`
/// and carries on does count: the `down` it runs is not confirmed and purges nothing.
#[test]
fn only_the_down_process_itself_is_left_out() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.path("app");
    let stack = bin();
    let out = fixture.ok(&["exec", "--", &stack, "-C", app.to_str().unwrap(), "--json", "down"]);
    let down: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(down["data"]["confirmed"], true, "{down}");

    fixture.ok(&["up"]);
    let script = format!("'{stack}' -C '{}' --json down --purge; echo; echo carried-on", app.display());
    let out = fixture.ok(&["exec", "--", "/bin/sh", "-c", &script]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let down: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(down["error"]["code"], "stop_unconfirmed", "{text}");
    assert_eq!(down["error"]["details"][0]["program"], "sh", "{text}");
    assert!(text.contains("carried-on"), "{text}");
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// `stack run` and MCP `stack_exec` calls are commands like any other, each reported by its own
/// process and age, never by the MCP server's. A server serving two checkouts reports each
/// command only in its own checkout; once a call is answered, the server no longer counts.
#[test]
fn run_tasks_and_mcp_calls_are_reported_per_command_and_per_checkout() {
    let reaper = Reaper::default();
    // The fake mise runs the task `hang` as a 30-second `sleep` of its own.
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.hang]\nrun = 'true'\n");
    let other = fixture.add_project("other");
    fixture.ok(&["up"]);
    let run = reaper.child(fixture.command(&["run", "hang"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let task = loop {
        let (_, status) = fixture.json(&["status"]);
        if let Some(pid) = status["data"]["commands"][0]["pid"].as_u64() {
            assert_eq!(status["data"]["commands"][0]["started_by"], "run hang", "{status}");
            break pid as u32;
        }
        assert!(Instant::now() < deadline, "{status}");
        thread::sleep(Duration::from_millis(20));
    };
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], task);
    assert!(reaper.running(run), "down signalled the task");
    // SAFETY: kill only sends a signal, to this test's own child; `stack run` passes it on.
    unsafe { libc::kill(run as i32, libc::SIGTERM) };
    reaper.wait(run);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);

    // One server, one call in flight for each checkout in turn.
    let holder = std::env::current_exe().unwrap();
    let pidfile = fixture.path("mcp.pid");
    let mut server = fixture.command(&["mcp"]).env("HOLD_SECS", "4").env("HOLD_PIDFILE", &pidfile).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let server_pid = server.id();
    let mut input = server.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(server.stdout.take().unwrap());
    reaper.child(server);
    use std::io::{BufRead, Write};
    for (i, (dir, idle)) in [(other.clone(), fixture.path("app")), (fixture.path("app"), other.clone())].into_iter().enumerate() {
        let _ = fs::remove_file(&pidfile);
        let call = json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":"stack_exec","arguments":{
            "dir": dir, "command": [holder, "--exact", "holder", "--ignored", "--test-threads", "1", SECRET_ARG]}}});
        writeln!(input, "{call}").unwrap();
        let held = reaper.pid(pid_in(&pidfile));
        // The idle checkout sees nothing of it.
        let (code, down) = fixture.json_at(&idle, &["down"]);
        assert_confirmed(code, &down);
        assert!(alive(held), "a down in another checkout touched the call's command");
        // Its own checkout reports the command, by its own PID and age, as an MCP call.
        let (code, down) = fixture.json_at(&dir, &["down"]);
        assert_unconfirmed(code, &down);
        let details = down["error"]["details"].as_array().unwrap();
        assert_eq!(details.len(), 1, "{down}");
        assert_eq!((details[0]["pid"].as_u64(), details[0]["started_by"].as_str()), (Some(u64::from(held)), Some("mcp stack_exec")), "{down}");
        assert!(details[0]["running_secs"].as_u64().unwrap() < 4, "its own age, not the server's: {down}");
        let hint = down["error"]["hint"].as_str().unwrap();
        assert!(!hint.contains(&server_pid.to_string()), "{hint}");
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["result"]["structuredContent"]["ok"], true, "{line}");
        let (code, down) = fixture.json_at(&dir, &["down"]);
        assert_confirmed(code, &down);
    }
    drop(input);
    assert_eq!(reaper.wait(server_pid), Some(0));
}

/// A Stack process an older Stack recorded in the session as running a command (by PID alone,
/// no mark, no registry): the first `down` reports that process, and only it, with a hint to
/// wait for it or run the command again under this Stack. The session goes with that `down`,
/// so a later `down` no longer knows of it (a documented limit).
#[test]
fn a_command_an_older_stack_started_is_known_only_to_the_first_down() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let older = reaper.child(Command::new("/bin/sleep").arg("60").spawn().unwrap());
    for path in [fixture.path("app/.stack/session.json")].into_iter().chain(fs::read_dir(fixture.path("state/sessions")).unwrap().map(|e| e.unwrap().path())) {
        let mut session: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        session["active_executions"]["e".repeat(64)] = json!(older);
        fs::write(&path, session.to_string()).unwrap();
    }
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    let details = down["error"]["details"].as_array().unwrap();
    assert_eq!(details.len(), 1, "{down}");
    assert_eq!((details[0]["pid"].as_u64(), details[0]["started_by"].as_str(), details[0]["kind"].as_str()), (Some(u64::from(older)), Some("older stack"), Some("command")), "{down}");
    let hint = down["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("restart that session under this Stack") && !hint.contains(&older.to_string()), "names no PID to stop: {hint}");
    assert!(reaper.running(older), "down signalled it");
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// Commands belong to the checkout's directory, not its path: one running in another checkout
/// is never reported or touched; one running in a directory that has since been replaced at the
/// same path is the old directory's; and one running in a checkout moved with `stack mv` is
/// still that checkout's.
#[test]
fn commands_follow_the_checkout_directory_not_its_path() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    fixture.ok(&["up"]);
    fixture.ok_at(&other, &["up"]);
    let pidfile = fixture.path("other.pid");
    let exec = reaper.child(fixture.hold_at(&other, "sleep", 60, &pidfile));
    let held = reaper.pid(pid_in(&pidfile));
    assert_eq!(fixture.json(&["status"]).1["data"]["commands"], json!([]));
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    assert!(alive(held) && reaper.running(exec), "down in one checkout touched another's command");

    // Moved by hand and told with `stack mv`: still the moved checkout's.
    let moved = fixture.path("moved");
    fs::rename(&other, &moved).unwrap();
    fixture.ok_at(&fixture.path("app"), &["mv", other.to_str().unwrap(), moved.to_str().unwrap()]);
    let (code, down) = fixture.json_at(&moved, &["down"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], held);

    // A new directory at the old path is another checkout.
    fs::create_dir(&other).unwrap();
    fs::write(other.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
    fixture.ok_at(&other, &["compile"]);
    assert_eq!(fixture.json_at(&other, &["status"]).1["data"]["commands"], json!([]));
    let (code, down) = fixture.json_at(&other, &["down"]);
    assert_confirmed(code, &down);

    end(held);
    reaper.wait(exec);
    let (code, down) = fixture.json_at(&moved, &["down"]);
    assert_confirmed(code, &down);
}

/// A registry that cannot be read makes every `down` unconfirmed with the reason (and `status`
/// says so); one that cannot be written refuses to run the command at all.
#[test]
fn a_registry_that_cannot_be_read_or_written_never_confirms_and_never_runs_unregistered() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("sleep", 60, &pidfile));
    let holder = reaper.pid(pid_in(&pidfile));
    let file = registry(&fixture, &fixture.path("app"));
    fs::write(&file, "{ not json").unwrap();
    for _ in 0..2 {
        let (code, down) = fixture.json(&["down"]);
        assert_unconfirmed(code, &down);
        assert_eq!(down["error"]["details"][0]["kind"], "registry", "{down}");
        assert!(down["error"]["message"].as_str().unwrap().contains("cannot be read"), "{down}");
    }
    let (_, status) = fixture.json(&["status"]);
    assert!(status["data"]["commands_error"].as_str().unwrap().contains("cannot be read"), "{status}");
    end(holder);
    reaper.wait(exec);
    fs::remove_file(&file).unwrap();
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);

    let commands = fixture.path("state/commands");
    fs::set_permissions(&commands, fs::Permissions::from_mode(0o500)).unwrap();
    let marker = fixture.path("ran");
    let out = fixture.command(&["--json", "exec", "--", "/usr/bin/touch", marker.to_str().unwrap()]).output().unwrap();
    fs::set_permissions(&commands, fs::Permissions::from_mode(0o755)).unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert!(!marker.exists(), "a command that could not be registered ran");
}

/// Every command Stack starts gets the checkout's command mark: one per checkout, never its
/// token. `stack env` gives none, and `[env]` cannot set it.
#[test]
fn commands_carry_their_checkouts_mark_and_env_does_not() {
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    let mark = |dir: &Path| {
        let (_, exec) = fixture.json_at(dir, &["exec", "--", "/usr/bin/printenv", "STACK_COMMAND_MARK"]);
        exec["data"]["stdout"].as_str().unwrap().trim().to_string()
    };
    let app = fixture.path("app").canonicalize().unwrap();
    let mine = mark(&app);
    assert_eq!(mine, stack::checkout::command_mark(&app).unwrap());
    assert_ne!(mine, stack::checkout::token(&app).unwrap());
    assert_ne!(mine, mark(&other));
    let (_, env) = fixture.json(&["env"]);
    assert!(env["data"]["env"].get("STACK_COMMAND_MARK").is_none(), "{env}");

    fs::write(fixture.path("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[env]\nSTACK_COMMAND_MARK='x'\n").unwrap();
    let (code, compiled) = fixture.json(&["compile"]);
    assert_eq!((code, &compiled["error"]["code"]), (Some(70), &json!("invalid_env")), "{compiled}");
}

/// macOS hides the environment of its own binaries: a `/bin/sleep` that a shell command left
/// running after it exited is not seen (the documented limit), and `down` confirms without
/// signalling it.
#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_orphan_is_not_seen_and_never_signalled() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let pidfile = fixture.path("orphan.pid");
    let script = format!("/bin/sleep 60 >/dev/null 2>&1 & echo $! > '{}'", pidfile.display());
    fixture.ok(&["exec", "--", "/bin/sh", "-c", &script]);
    let orphan = reaper.pid(pid_in(&pidfile));
    if stack::procinfo::environ_has(orphan, "STACK_COMMAND_MARK", &stack::checkout::command_mark(&fixture.path("app")).unwrap()) == Some(true) {
        eprintln!("skipping: this machine shows /bin/sleep's environment");
        return;
    }
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    assert!(alive(orphan), "down signalled a process it could not see");
}

/// A leftover process of this checkout's service (its token, its port, leading its group, no
/// record) shares its group with its worker (no token) and another checkout's registered `stack
/// exec`: the group is not signalled; the leftover and its worker are stopped one by one and the
/// other checkout's command is untouched.
#[test]
fn a_leftover_group_holding_another_checkouts_command_is_not_signalled_as_a_group() {
    use std::os::unix::process::CommandExt;
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let other = fixture.add_project("other");
    let (_, inspect) = fixture.json(&["inspect"]);
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap();
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let holder = std::env::current_exe().unwrap();
    let leftover_file = fixture.path("leftover.pid");
    let leftover = reaper.child(
        Command::new(&holder)
            .args(["--exact", "holder", "--ignored", "--test-threads", "1"])
            .env("HOLD_MODE", "listen")
            .env("HOLD_PORT", port.to_string())
            .env("HOLD_PIDFILE", &leftover_file)
            .env(stack::checkout::TOKEN_VAR, &token)
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    pid_in(&leftover_file);
    // The service's worker, in its group, without the token.
    let worker_file = fixture.path("worker.pid");
    let worker = reaper.child(
        Command::new(&holder)
            .args(["--exact", "holder", "--ignored", "--test-threads", "1"])
            .env("HOLD_SECS", "60")
            .env("HOLD_PIDFILE", &worker_file)
            .process_group(leftover as i32)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    pid_in(&worker_file);
    let pidfile = fixture.path("other.pid");
    let exec = {
        let mut command = fixture.command_at(&other, &["exec", "--", holder.to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).process_group(leftover as i32).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    let held = reaper.pid(pid_in(&pidfile));
    assert_eq!(stack::procinfo::process(exec).unwrap().group, leftover, "the other checkout's Stack process shares the leftover's group");
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    assert_eq!(reaper.wait(leftover), None, "the leftover was stopped by a signal");
    assert_eq!(reaper.wait(worker), None, "its worker without the token was stopped too");
    assert!(reaper.running(exec) && alive(held), "another checkout's command in that group was signalled");
    end(held);
    reaper.wait(exec);
}

/// A leftover of this checkout's service (its token, its port, leading its group) shares its
/// group with another checkout's registered `stack exec`, so the group is stopped member by
/// member, and starts a worker (no token, no mark) in that group on its first stop signal, then
/// exits. A worker that stays in the group is stopped; one that leaves the group on its own first
/// signal is no longer known to be the checkout's: it is left running and `down` is not
/// confirmed. The other checkout's command survives both.
#[test]
fn a_late_worker_of_a_mixed_group_is_stopped_only_while_it_stays_in_the_group() {
    use std::os::unix::process::CommandExt;
    for worker_mode in ["sleep", "leave"] {
        let reaper = Reaper::default();
        let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
        let other = fixture.add_project("other");
        let (_, inspect) = fixture.json(&["inspect"]);
        let port = inspect["data"]["ports"]["web"].as_u64().unwrap();
        let token = stack::checkout::token(&fixture.path("app")).unwrap();
        let holder = std::env::current_exe().unwrap();
        let (leftover_file, worker_file) = (fixture.path("leftover.pid"), fixture.path("worker.pid"));
        let leftover = reaper.child(
            Command::new(&holder)
                .args(["--exact", "holder", "--ignored", "--test-threads", "1"])
                .env("HOLD_MODE", "listen")
                .env("HOLD_PORT", port.to_string())
                .env("HOLD_PIDFILE", &leftover_file)
                .env("HOLD_SECS", "60")
                .env("HOLD_WORKER", worker_mode)
                .env("HOLD_WORKER_PIDFILE", &worker_file)
                .env(stack::checkout::TOKEN_VAR, &token)
                .process_group(0)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        pid_in(&leftover_file);
        let pidfile = fixture.path("other.pid");
        let exec = {
            let mut command = fixture.command_at(&other, &["exec", "--", holder.to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
            command.env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).process_group(leftover as i32).stdout(Stdio::null()).stderr(Stdio::null());
            reaper.child(command.spawn().unwrap())
        };
        let held = reaper.pid(pid_in(&pidfile));
        let (code, down) = fixture.json(&["down"]);
        let worker = reaper.pid(pid_in(&worker_file));
        assert_eq!(reaper.wait(leftover), Some(0), "{worker_mode}: the leftover exits on its own once its worker runs");
        if worker_mode == "sleep" {
            assert_confirmed(code, &down);
            assert!(!alive(worker), "the worker that stayed in the group was not stopped");
        } else {
            assert_unconfirmed(code, &down);
            let detail = naming(&down["error"]["details"], worker);
            assert!(detail["unrecorded"] == true && detail["unproven"] == true, "{down}");
            assert!(detail["reason"].as_str().unwrap().contains("can no longer be shown to belong to this checkout"), "{down}");
            assert!(down["error"]["message"].as_str().unwrap().contains("could not be shown to belong to it"), "{down}");
            assert!(down["error"]["hint"].as_str().unwrap().contains("check what it is before stopping it"), "{down}");
            assert!(alive(worker), "the worker that left the group was signalled");
            assert_ne!(stack::procinfo::process(worker).unwrap().group, leftover, "it left the group");
            // Nothing else shows it now, but `down` was not confirmed: `status` says so until a
            // `down` confirms.
            assert_marked_unconfirmed(&fixture);
            end(worker);
            assert_confirmed_then_not_started(&fixture);
        }
        assert!(reaper.running(exec) && alive(held), "{worker_mode}: another checkout's command in that group was signalled");
        end(held);
        reaper.wait(exec);
    }
}

/// After a `down` that was not confirmed: `status` (no session) is started, exits 1 (a service is
/// not verified) and says the last `down` was not confirmed.
fn assert_marked_unconfirmed(fixture: &Fixture) {
    let (code, status) = fixture.json(&["status"]);
    assert_eq!((code, &status["data"]["started"], &status["passed"]), (Some(1), &json!(true), &json!(false)), "{status}");
    let found = status["data"]["found"].to_string();
    assert!(found.contains("the last `stack down` here") && found.contains("was not confirmed") && found.contains("run `stack down` again"), "{status}");
    let out = fixture.command(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with("no session") && text.contains("found        the last `stack down` here"), "{text}");
}

/// Once a `down` confirms: not started.
fn assert_confirmed_then_not_started(fixture: &Fixture) {
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    let (code, status) = fixture.json(&["status"]);
    assert_eq!((code, &status["data"]["started"], status["data"].get("found")), (Some(0), &json!(false), None), "{status}");
}

/// A `down` that confirms (and purges) but cannot remove an earlier unconfirmed `down`'s marker
/// (here a directory stands where it is) still returns its confirmed result, purge included,
/// with a warning naming the marker; `status` keeps saying the last `down` was unconfirmed until
/// the marker is gone.
#[test]
fn a_marker_that_cannot_be_removed_does_not_hide_a_confirmed_purge() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let identity = stack::checkout::identity(&fixture.path("app")).unwrap();
    let marker = fixture.path("state/unconfirmed").join(format!("{identity}.json"));
    fs::create_dir_all(marker.join("held")).unwrap();
    let (code, down) = fixture.json(&["down", "--purge"]);
    assert_confirmed(code, &down);
    assert!(down["data"]["purge"].is_object(), "{down}");
    let warning = down["data"]["warnings"][0].as_str().unwrap_or_else(|| panic!("{down}"));
    assert!(warning.contains(&marker.display().to_string()) && warning.contains("could not be removed"), "{warning}");
    let out = fixture.command(&["down"]).output().unwrap();
    assert!(out.status.success() && String::from_utf8_lossy(&out.stderr).contains("warning: the marker of an earlier unconfirmed"), "{out:?}");
    let (code, status) = fixture.json(&["status"]);
    assert_eq!((code, &status["data"]["started"]), (Some(1), &json!(true)), "{status}");
    assert!(status["data"]["found"][0].as_str().unwrap().contains("cannot tell whether the last `stack down` here was confirmed"), "{status}");
    fs::remove_dir_all(&marker).unwrap();
    let (code, status) = fixture.json(&["status"]);
    assert_eq!((code, &status["data"]["started"]), (Some(0), &json!(false)), "{status}");
}

/// A detached child of a `stack exec` command, which `down` reported (found by its command
/// mark), then replaced itself with `/bin/sleep` under the same PID: on macOS its environment is
/// hidden from then on, so no search finds it (elsewhere the search still may). Either way the
/// unconfirmed `down` keeps `status` started until a `down` confirms.
#[test]
fn status_stays_started_after_an_unconfirmed_down_whose_process_hid_its_environment() {
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let (pidfile, trigger) = (fixture.path("holder.pid"), fixture.path("trigger"));
    let holder = std::env::current_exe().unwrap();
    let exec = reaper.child(
        fixture
            .command(&["exec", "--", holder.to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"])
            .env("HOLD_MODE", "group")
            .env("HOLD_CHILD_MODE", "exec")
            .env("HOLD_TRIGGER", &trigger)
            .env("HOLD_PIDFILE", &pidfile)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert_eq!(reaper.wait(exec), Some(0));
    let child = reaper.pid(pid_in(&pidfile));
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    assert_eq!(naming(&down["error"]["details"], child)["found_by"], "mark", "{down}");
    fs::write(&trigger, "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while stack::procinfo::named(child).map(|n| n.comm) != Some("sleep".into()) {
        assert!(Instant::now() < deadline, "{child} never became /bin/sleep");
        thread::sleep(Duration::from_millis(10));
    }
    let mark = stack::checkout::command_mark(&fixture.path("app")).unwrap();
    if stack::procinfo::environ_has(child, stack::checkout::COMMAND_VAR, &mark) == Some(true) {
        eprintln!("note: this machine shows /bin/sleep's environment; the search finds it too");
    }
    assert_marked_unconfirmed(&fixture);
    end(child);
    assert_confirmed_then_not_started(&fixture);
}

/// Another checkout's registered command (and the Stack process running it) inherited this
/// checkout's service token: `down` here is not confirmed, reports them as another checkout's,
/// and names neither in its hint's PIDs, pointing at that checkout instead.
#[test]
fn another_checkouts_command_holding_this_token_is_reported_as_such() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    fixture.ok(&["up"]);
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let pidfile = fixture.path("other.pid");
    let exec = {
        let mut command = fixture.command_at(&other, &["exec", "--", std::env::current_exe().unwrap().to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env(stack::checkout::TOKEN_VAR, &token).env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    let held = reaper.pid(pid_in(&pidfile));
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    for pid in [held, exec] {
        assert_eq!(naming(&down["error"]["details"], pid)["another_checkout"], true, "{down}");
    }
    let hint = down["error"]["hint"].as_str().unwrap();
    assert!(!hint.contains(&held.to_string()) && !hint.contains(&exec.to_string()) && hint.contains("look at that checkout"), "{hint}");
    assert!(reaper.running(exec) && alive(held), "down signalled another checkout's command");
    end(held);
    reaper.wait(exec);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// A leftover listener on this checkout's port that carries its token and a command mark no
/// registry is for is no command: `down` stops it and confirms.
#[test]
fn a_leftover_carrying_an_arbitrary_mark_is_stopped() {
    let reaper = Reaper::default();
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let (_, inspect) = fixture.json(&["inspect"]);
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap();
    let pidfile = fixture.path("leftover.pid");
    let leftover = reaper.child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "holder", "--ignored", "--test-threads", "1"])
            .env("HOLD_MODE", "listen")
            .env("HOLD_PORT", port.to_string())
            .env("HOLD_PIDFILE", &pidfile)
            .env(stack::checkout::TOKEN_VAR, stack::checkout::token(&fixture.path("app")).unwrap())
            .env(stack::checkout::COMMAND_VAR, "0123456789abcdef0123456789abcdef")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    pid_in(&pidfile);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
    assert_eq!(reaper.wait(leftover), None, "stopped by a signal");
}

/// The race in `stack exec -- stack down`: the inner `down` looks before the outer process
/// recorded its PID. A `down` that is the direct child of a not-yet-recorded command's Stack
/// process is that command; a grandchild (a shell's `down`) is not.
#[test]
fn a_down_recognises_itself_before_its_pid_is_recorded() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.path("app");
    let me = stack::procinfo::process(std::process::id()).unwrap();
    let registry = registry(&fixture, &app);
    let entry = json!([{ "id": "pending", "started_by": "exec", "program": "stack", "registered_at_ms": 0,
        "coordinator": { "pid": me.pid, "started": me.started, "boot": stack::procinfo::boot_id() } }]);
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(&registry, entry.to_string()).unwrap();
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);

    fs::write(&registry, entry.to_string()).unwrap();
    // `; true` keeps the shell from replacing itself with its last command.
    let script = format!("'{}' -C '{}' --json down; true", bin(), app.display());
    let out = Command::new("/bin/sh")
        .args(["-c", &script])
        .env("STACK_STATE_DIR", fixture.path("state"))
        .env("STACK_CACHE_DIR", fixture.path("cache"))
        .env("PATH", format!("{}:{}", fixture.path("bin").display(), std::env::var("PATH").unwrap()))
        .env("REVIEW_FIXTURE", fixture.dir.path())
        .env("REVIEW_PYTHON", python::python())
        .output()
        .unwrap();
    let down: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(down["error"]["code"], "stop_unconfirmed", "{down}");
    fs::remove_file(&registry).unwrap();
}

/// A provider's environment that sets `STACK_COMMAND_MARK` never reaches `stack env`, and a
/// command gets the checkout's own mark.
#[test]
fn a_provider_supplied_mark_is_stripped() {
    let fixture = Fixture::new();
    let path = format!("{}:{}", fixture.path("bin").display(), std::env::var("PATH").unwrap());
    fs::write(fixture.path("env.json"), json!({ "PATH": path, "STACK_COMMAND_MARK": "from-the-provider" }).to_string()).unwrap();
    let (_, env) = fixture.json(&["env"]);
    assert!(!env.to_string().contains("from-the-provider"), "{env}");
    let (_, exec) = fixture.json(&["exec", "--", "/usr/bin/printenv", "STACK_COMMAND_MARK"]);
    assert_eq!(exec["data"]["stdout"].as_str().unwrap().trim(), stack::checkout::command_mark(&fixture.path("app")).unwrap(), "{exec}");
}

/// A service ran `stack exec`, whose command started a helper in a session of its own and
/// exited: the helper keeps the checkout's command mark and the service token. With no live
/// registered command left (and even with no registry file at all), `down` never signals it and
/// reports it.
#[test]
fn a_detached_helper_of_a_command_a_service_ran_is_never_signalled() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let pidfile = fixture.path("helper.pid");
    let exec = {
        let mut command = fixture.command(&["exec", "--", std::env::current_exe().unwrap().to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env(stack::checkout::TOKEN_VAR, &token).env("HOLD_MODE", "session").env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    assert_eq!(reaper.wait(exec), Some(0));
    let helper = reaper.pid(pid_in(&pidfile));
    for _ in 0..2 {
        let (code, down) = fixture.json(&["down"]);
        assert_unconfirmed(code, &down);
        assert_eq!(naming(&down["error"]["details"], helper)["found_by"], "mark", "{down}");
        assert!(alive(helper), "down signalled the helper");
        // The checkout's own mark needs no registry.
        let _ = fs::remove_file(registry(&fixture, &fixture.path("app")));
    }
    end(helper);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// The same helper started by another checkout's command, holding this checkout's token: after
/// that checkout's last command ended, its registry's lock file still makes its mark known, so
/// here the helper is never signalled, and is reported as another checkout's.
#[test]
fn another_checkouts_detached_helper_holding_this_token_is_never_signalled() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    fixture.ok(&["up"]);
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let pidfile = fixture.path("helper.pid");
    let exec = {
        let mut command = fixture.command_at(&other, &["exec", "--", std::env::current_exe().unwrap().to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env(stack::checkout::TOKEN_VAR, &token).env("HOLD_MODE", "session").env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    assert_eq!(reaper.wait(exec), Some(0));
    let helper = reaper.pid(pid_in(&pidfile));
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    let detail = naming(&down["error"]["details"], helper);
    assert_eq!(detail["another_checkout"], true, "{down}");
    assert!(!down["error"]["hint"].as_str().unwrap().contains(&helper.to_string()), "{down}");
    assert!(alive(helper), "down signalled another checkout's helper");
    end(helper);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}

/// A registry that cannot be read: `down` signals nothing outside the services' own recorded
/// groups (here a command carrying the service token, which a search for leftovers would
/// otherwise look at), and reports the registry error.
#[test]
fn with_an_unreadable_registry_down_signals_nothing_beyond_the_services() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let token = stack::checkout::token(&fixture.path("app")).unwrap();
    let pidfile = fixture.path("holder.pid");
    let exec = {
        let mut command = fixture.command(&["exec", "--", std::env::current_exe().unwrap().to_str().unwrap(), "--exact", "holder", "--ignored", "--test-threads", "1"]);
        command.env(stack::checkout::TOKEN_VAR, &token).env("HOLD_SECS", "60").env("HOLD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        reaper.child(command.spawn().unwrap())
    };
    let holder = reaper.pid(pid_in(&pidfile));
    fs::write(registry(&fixture, &fixture.path("app")), "{ not json").unwrap();
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    assert_eq!(down["error"]["details"][0]["kind"], "registry", "{down}");
    assert!(reaper.running(exec) && alive(holder), "down signalled a process while its registry could not be read");
    end(holder);
    reaper.wait(exec);
}

/// An entry whose process was never recorded (its Stack process gone) keeps `down`
/// unconfirmed; `status` still lists the other, live commands beside the error; `down
/// --forget-unrecorded` removes that entry alone, and the live command still counts.
#[test]
fn forget_unrecorded_removes_only_unrecorded_entries() {
    let reaper = Reaper::default();
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let pidfile = fixture.path("holder.pid");
    let exec = reaper.child(fixture.hold("sleep", 60, &pidfile));
    let holder = reaper.pid(pid_in(&pidfile));
    let file = registry(&fixture, &fixture.path("app"));
    let mut entries: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut dead = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = dead.id();
    dead.wait().unwrap();
    entries.as_array_mut().unwrap().push(json!({ "id": "lost", "started_by": "exec", "program": "lost", "registered_at_ms": 0,
        "coordinator": { "pid": dead_pid, "started": u64::MAX, "boot": stack::procinfo::boot_id() } }));
    fs::write(&file, entries.to_string()).unwrap();
    let (_, status) = fixture.json(&["status"]);
    naming(&status["data"]["commands"], holder);
    assert!(status["data"]["commands_error"].as_str().unwrap().contains("--forget-unrecorded"), "{status}");
    let (code, down) = fixture.json(&["down"]);
    assert_unconfirmed(code, &down);
    assert_eq!(down["error"]["details"][0]["kind"], "registry", "{down}");
    assert!(down["error"]["hint"].as_str().unwrap().contains("--forget-unrecorded"), "{down}");
    let (code, down) = fixture.json(&["down", "--purge", "--forget-unrecorded"]);
    assert_unconfirmed(code, &down);
    naming(&down["error"]["details"], holder);
    end(holder);
    reaper.wait(exec);
    let (code, down) = fixture.json(&["down"]);
    assert_confirmed(code, &down);
}
