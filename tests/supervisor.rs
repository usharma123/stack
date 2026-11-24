#![cfg(unix)]
//! Stack's own supervisor (`stack __supervise <dir>`), driven directly through the files it
//! reads and writes: `spec.json` (what should run) and `state.json` (what runs).

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[path = "support/python.rs"]
mod python;

struct Supervisor {
    dir: tempfile::TempDir,
    revision: std::cell::Cell<u64>,
}

impl Supervisor {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap(), revision: std::cell::Cell::new(0) }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("sup")
    }

    /// The checkout token this test's services carry, as Stack gives every service its
    /// checkout's.
    fn token(&self) -> String {
        format!("test-{}", self.dir.path().file_name().unwrap().to_string_lossy())
    }

    /// A service running `script` through `sh -c`, wanted, with the test's PATH.
    fn service(&self, script: &str) -> Value {
        json!({
            "argv": ["/bin/sh", "-c", script],
            "env": { "PATH": std::env::var("PATH").unwrap(), "OUT": self.dir.path(), "PY": python::python(), "STACK_CHECKOUT_TOKEN": self.token() },
            "cwd": self.dir.path(),
            "port": 1,
            "ready": { "kind": "spawned" },
            "stop_signal": libc::SIGTERM,
            "want": true,
            "start_seq": 1,
        })
    }

    /// Write `services` as the spec (one revision more) and tell a running supervisor.
    fn spec(&self, services: Value, shutdown: bool) {
        self.revision.set(self.revision.get() + 1);
        fs::create_dir_all(self.path()).unwrap();
        let spec = json!({ "revision": self.revision.get(), "shutdown": shutdown, "token": self.token(), "services": services });
        fs::write(self.path().join("spec.json"), spec.to_string()).unwrap();
        self.wake();
    }

    /// Send the recorded supervisor SIGHUP, while it is still the recorded process.
    fn wake(&self) {
        let state = self.state();
        let (Some(pid), Some(started)) = (state["supervisor"]["pid"].as_u64(), state["supervisor"]["started"].as_u64()) else { return };
        if stack::procinfo::process(pid as u32).is_some_and(|p| p.started == started) {
            unsafe { libc::kill(pid as i32, libc::SIGHUP) };
        }
    }

    /// Start a supervisor for the directory; it detaches at once. `STACK_TEST_BIN` runs another
    /// build's (to show a regression test fails without its fix).
    fn start(&self) {
        let bin = std::env::var("STACK_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_stack").to_string());
        let status = Command::new(bin).arg("__supervise").arg(self.path()).env_clear().status().unwrap();
        assert!(status.success());
    }

    /// [`Self::start`] with its output appended to `supervisor.log`, as Stack starts one.
    fn start_logged(&self) {
        let bin = std::env::var("STACK_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_stack").to_string());
        fs::create_dir_all(self.path()).unwrap();
        let log = fs::OpenOptions::new().create(true).append(true).open(self.path().join("supervisor.log")).unwrap();
        let status = Command::new(bin).arg("__supervise").arg(self.path()).env_clear().stdout(log.try_clone().unwrap()).stderr(log).status().unwrap();
        assert!(status.success());
    }

    fn file_pid(&self, name: &str) -> u32 {
        let path = self.dir.path().join(name);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(pid) = fs::read_to_string(&path).ok().and_then(|t| t.trim().parse().ok()) {
                return pid;
            }
            assert!(Instant::now() < deadline, "{name} never written");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn supervisor_pid(&self) -> u32 {
        self.state()["supervisor"]["pid"].as_u64().unwrap() as u32
    }

    /// Kill the running supervisor outright, as a crash would.
    fn kill_supervisor(&self) {
        let pid = self.supervisor_pid();
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        assert!(gone_soon(pid));
    }

    fn state(&self) -> Value {
        fs::read(self.path().join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
    }

    /// Wait (up to `secs`) until `done` holds for the state.
    fn until(&self, secs: u64, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let state = self.state();
            if done(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "{what}: {state}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn pid(&self, service: &str) -> Option<u32> {
        self.state()["services"][service]["process"]["pid"].as_u64().map(|p| p as u32)
    }

    fn shutdown(&self) {
        self.spec(json!({}), true);
        self.until(20, "the supervisor exits", |s| s["supervisor"].is_null());
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // A supervisor whose spec is gone stops everything and exits.
        let _ = fs::remove_file(self.path().join("spec.json"));
        self.wake();
    }
}

fn alive(pid: u32) -> bool {
    // A zombie is gone too.
    stack::procinfo::process(pid).is_some()
}

fn gone_soon(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        if Instant::now() > deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

#[test]
fn services_start_together_and_the_state_names_each_process_by_pid_and_start_time() {
    let sup = Supervisor::new();
    // Each takes a second to become itself; started one after another they would take three.
    let slow = |name: &str| sup.service(&format!("sleep 1; echo {name} >\"$OUT/{name}\"; exec sleep 600"));
    sup.spec(json!({ "a": slow("a"), "b": slow("b"), "c": slow("c") }), false);
    let started = Instant::now();
    sup.start();
    let marks = ["a", "b", "c"].map(|n| sup.dir.path().join(n));
    while !marks.iter().all(|m| m.exists()) {
        assert!(started.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() < Duration::from_millis(2500), "started one after another: {:?}", started.elapsed());
    let state = sup.state();
    assert_eq!(state["applied"], 1);
    for name in ["a", "b", "c"] {
        let s = &state["services"][name];
        assert_eq!(s["status"], "running", "{state}");
        let pid = s["process"]["pid"].as_u64().unwrap() as u32;
        assert_eq!(s["process"]["started"], stack::procinfo::process(pid).unwrap().started, "{state}");
        // Each leads its own process group.
        assert_eq!(s["group"], pid, "{state}");
    }
    sup.shutdown();
    assert!(state["services"].as_object().unwrap().values().all(|s| !alive(s["process"]["pid"].as_u64().unwrap() as u32)));
}

#[test]
fn a_stop_signals_the_whole_group_with_the_services_signal_then_kills_it() {
    let sup = Supervisor::new();
    // A Postgres-like service: stops on SIGINT only, and has a child in its group.
    let mut pg = sup.service(r#"trap 'echo int >"$OUT/int"; exit 0' INT; trap '' TERM; sleep 600 & echo $! >"$OUT/child"; while :; do sleep 0.05; done"#);
    pg["stop_signal"] = json!(libc::SIGINT);
    // A service that ignores its stop signal.
    let stubborn = sup.service(r#"trap '' TERM INT; while :; do sleep 0.05; done"#);
    sup.spec(json!({ "pg": pg, "stubborn": stubborn }), false);
    sup.start();
    sup.until(10, "both run", |s| s["services"]["pg"]["status"] == "running" && s["services"]["stubborn"]["status"] == "running");
    let child_path = sup.dir.path().join("child");
    while !child_path.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    let child: u32 = fs::read_to_string(&child_path).unwrap().trim().parse().unwrap();
    let (pg_pid, stubborn_pid) = (sup.pid("pg").unwrap(), sup.pid("stubborn").unwrap());
    let started = Instant::now();
    sup.spec(json!({}), true);
    let state = sup.until(25, "the supervisor exits", |s| s["supervisor"].is_null());
    assert!(sup.dir.path().join("int").exists(), "pg got SIGINT");
    assert!(!alive(pg_pid) && !alive(child), "the group went with its leader");
    assert!(!alive(stubborn_pid));
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(9) && took < Duration::from_secs(20), "killed after the grace period: {took:?}");
    assert_eq!(state["services"]["pg"]["status"], "stopped", "{state}");
    assert_eq!(state["services"]["stubborn"]["exit"], format!("signal {}", libc::SIGKILL), "{state}");
}

#[test]
fn a_service_that_exits_is_restarted_only_as_its_policy_allows_or_when_asked() {
    let sup = Supervisor::new();
    // Fails at once every time, counting its runs.
    let flaky = |retries: u32| {
        let mut s = sup.service("echo run >>\"$OUT/runs\"; exit 3");
        s["retries"] = json!(retries);
        s
    };
    sup.spec(json!({ "flaky": flaky(2) }), false);
    sup.start();
    // Runs once, then twice more after growing delays (1s, 2s), then stays failed.
    let state = sup.until(15, "retries used up", |s| s["services"]["flaky"]["restarts"] == 2 && s["services"]["flaky"]["status"] == "failed");
    assert_eq!(state["services"]["flaky"]["exit"], "exit code 3", "{state}");
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(fs::read_to_string(sup.dir.path().join("runs")).unwrap().lines().count(), 3, "no fourth run");
    // Without retries (Stack's default), a failed service stays failed through later changes
    // of the spec, until a command asks for it again.
    let mut once = sup.service("echo once >>\"$OUT/once\"; exit 1");
    sup.spec(json!({ "flaky": flaky(2), "once": once.clone() }), false);
    sup.until(5, "once ran", |s| s["services"]["once"]["status"] == "failed");
    sup.spec(json!({ "flaky": flaky(2), "once": once.clone() }), false);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(fs::read_to_string(sup.dir.path().join("once")).unwrap().lines().count(), 1);
    once["start_seq"] = json!(2);
    sup.spec(json!({ "flaky": flaky(2), "once": once }), false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(sup.dir.path().join("once")).unwrap().lines().count() < 2 {
        assert!(Instant::now() < deadline, "asked again, it runs again");
        thread::sleep(Duration::from_millis(10));
    }
    sup.shutdown();
}

#[test]
fn readiness_holds_a_service_starting_until_its_port_or_command_answers() {
    let sup = Supervisor::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut by_port = sup.service(&format!(r#"sleep 0.5; exec "$PY" -c 'import socket,time; s=socket.socket(); s.bind(("127.0.0.1", {port})); s.listen(); time.sleep(600)'"#));
    by_port["ready"] = json!({ "kind": "port", "port": port });
    let mut by_command = sup.service("exec sleep 600");
    by_command["ready"] = json!({ "kind": "command", "command": "test -f ready" });
    sup.spec(json!({ "by_port": by_port, "by_command": by_command }), false);
    sup.start();
    let state = sup.until(5, "spawned", |s| s["services"]["by_command"]["process"].is_object());
    assert_eq!(state["services"]["by_command"]["status"], "starting", "{state}");
    sup.until(10, "the port answers", |s| s["services"]["by_port"]["status"] == "running");
    assert_eq!(sup.state()["services"]["by_command"]["status"], "starting");
    fs::write(sup.dir.path().join("ready"), "").unwrap();
    sup.until(10, "the command passes", |s| s["services"]["by_command"]["status"] == "running");
    sup.shutdown();
}

#[test]
fn services_a_killed_supervisor_left_are_stopped_by_the_next_one_by_pid_start_time_and_group() {
    let sup = Supervisor::new();
    sup.spec(json!({ "a": sup.service("sleep 600 & wait") }), false);
    sup.start();
    let state = sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let supervisor = state["supervisor"]["pid"].as_u64().unwrap() as u32;
    let a = sup.pid("a").unwrap();
    unsafe { libc::kill(supervisor as i32, libc::SIGKILL) };
    assert!(gone_soon(supervisor));
    // On Linux the kernel gives the service its stop signal when the supervisor dies; on macOS
    // it is left running, recorded.
    thread::sleep(Duration::from_millis(200));
    let orphaned = alive(a);
    assert!(orphaned || cfg!(target_os = "linux"), "only Linux stops it on its own");
    // The next supervisor stops what the last one left before it starts anything.
    sup.start();
    let state = sup.until(15, "a runs again", |s| s["services"]["a"]["status"] == "running" && s["services"]["a"]["process"]["pid"].as_u64() != Some(u64::from(a)));
    assert!(!alive(a), "the orphan was stopped");
    assert_ne!(state["supervisor"]["pid"].as_u64().unwrap() as u32, supervisor);
    sup.shutdown();
}

#[test]
fn a_stale_state_naming_another_process_is_never_signalled() {
    let sup = Supervisor::new();
    // A record from a crash long ago: the pid now names this test's own process, which started
    // at another time than recorded, and a supervisor pid nobody runs.
    let me = stack::procinfo::process(std::process::id()).unwrap();
    fs::create_dir_all(sup.path()).unwrap();
    let stale = json!({
        "supervisor": { "pid": me.pid, "started": me.started + 1 },
        "applied": 7,
        "services": { "a": { "status": "running", "process": { "pid": me.pid, "started": me.started + 1 }, "group": me.pid, "port": 1, "stop_signal": libc::SIGTERM } },
    });
    fs::write(sup.path().join("state.json"), stale.to_string()).unwrap();
    sup.spec(json!({ "a": sup.service("exec sleep 600") }), false);
    sup.start();
    let state = sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running" && s["services"]["a"]["process"]["pid"].as_u64() != Some(u64::from(me.pid)));
    assert_ne!(state["supervisor"]["pid"].as_u64().unwrap() as u32, me.pid);
    // This test is still alive, unsignalled.
    sup.shutdown();
}

#[test]
fn one_supervisor_per_directory_and_it_exits_when_its_directory_goes() {
    let sup = Supervisor::new();
    sup.spec(json!({ "a": sup.service("exec sleep 600") }), false);
    sup.start();
    let state = sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let first = state["supervisor"]["pid"].as_u64().unwrap() as u32;
    // A second one finds the first holding the directory's lock and leaves.
    sup.start();
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(sup.state()["supervisor"]["pid"].as_u64().unwrap() as u32, first);
    let a = sup.pid("a").unwrap();
    // Its state directory removed (Stack's state reset, a test's temporary directory): it stops
    // its services and exits.
    fs::remove_dir_all(sup.path()).unwrap();
    assert!(gone_soon(first) && gone_soon(a));
}

#[test]
fn output_is_kept_per_service_with_when_each_line_was_read() {
    let sup = Supervisor::new();
    sup.spec(json!({ "talk": sup.service("echo out; echo err >&2; exec sleep 600") }), false);
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    sup.start();
    let log = sup.path().join("logs/talk.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(&log).map_or(true, |t| t.lines().count() < 2) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let text = fs::read_to_string(&log).unwrap();
    let mut lines: Vec<(u128, &str)> = text.lines().map(|l| l.split_once(' ').map(|(t, r)| (t.parse().unwrap(), r)).unwrap()).collect();
    lines.sort_by_key(|(_, l)| *l);
    assert_eq!(lines.iter().map(|(_, l)| *l).collect::<Vec<_>>(), ["err", "out"]);
    assert!(lines.iter().all(|(t, _)| *t >= before), "{text}");
    sup.shutdown();
}

/// End a process the test started (through the supervisor) that a test expects to survive.
fn end(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

// ---- recovery after the supervisor was killed (council item 1) ----------------------------

/// A child that ignores SIGTERM and records its pid in `$OUT/child`. Python, not a platform
/// binary such as `sleep`, whose environment macOS shows nobody.
const STUBBORN: &str = r#""$PY" -c 'import os,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); open(os.environ["OUT"]+"/child","w").write(str(os.getpid())); time.sleep(600)'"#;

#[test]
fn recovery_stops_a_term_ignoring_member_of_a_group_whose_leader_still_runs() {
    let sup = Supervisor::new();
    // The leader exits on SIGTERM; its child ignores it and stays in the group.
    sup.spec(json!({ "a": sup.service(&format!("trap 'exit 0' TERM; {STUBBORN} & wait")) }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let child = sup.file_pid("child");
    let leader = sup.pid("a").unwrap();
    sup.kill_supervisor();
    // A new supervisor told to stop everything recovers what the dead one left first.
    sup.spec(json!({ "a": sup.service("true") }), true);
    let started = Instant::now();
    sup.start();
    sup.until(25, "the new supervisor stopped everything", |s| s["supervisor"].is_null() && s["applied"] == 2);
    assert!(!alive(leader) && !alive(child), "the member that ignored SIGTERM survived recovery");
    assert!(started.elapsed() >= Duration::from_secs(9), "killed only after the grace period");
}

#[test]
fn recovery_with_the_leader_already_gone_stops_the_groups_verified_members_by_pid() {
    let sup = Supervisor::new();
    sup.spec(json!({ "a": sup.service(&format!("{STUBBORN} & exec sleep 600")) }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let child = sup.file_pid("child");
    let leader = sup.pid("a").unwrap();
    sup.kill_supervisor();
    // The leader goes too, before anything recovers: only its child is left in the group.
    end(leader);
    assert!(gone_soon(leader));
    sup.spec(json!({ "a": sup.service("true") }), true);
    sup.start();
    sup.until(25, "the new supervisor stopped everything", |s| s["supervisor"].is_null() && s["applied"] == 2);
    assert!(!alive(child), "a group member left by a dead leader survived recovery");
}

#[test]
fn recovery_never_signals_a_group_member_it_cannot_show_is_the_checkouts_and_starts_nothing() {
    let sup = Supervisor::new();
    // The child drops the checkout's token: nothing shows it is this checkout's.
    sup.spec(json!({ "a": sup.service(&format!("env -u STACK_CHECKOUT_TOKEN {STUBBORN} & exec sleep 600")) }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let child = sup.file_pid("child");
    let leader = sup.pid("a").unwrap();
    sup.kill_supervisor();
    end(leader);
    assert!(gone_soon(leader));
    let before = sup.state();
    sup.spec(json!({ "a": sup.service("exec sleep 600") }), false);
    sup.start();
    thread::sleep(Duration::from_millis(1500));
    assert!(alive(child), "an unverifiable process was signalled");
    assert_eq!(sup.state(), before, "the record is kept as evidence and nothing new started");
    let log = fs::read_to_string(sup.path().join("supervisor.log")).unwrap_or_default();
    assert!(log.is_empty() || !log.contains("panicked"), "{log}");
    end(child);
}

// ---- readiness probes (council item 2) ------------------------------------------------------

#[test]
fn a_readiness_probe_is_killed_with_its_group_when_its_service_stops_and_shutdown_waits_for_it() {
    let sup = Supervisor::new();
    let mut a = sup.service("exec sleep 600");
    a["ready"] = json!({ "kind": "command", "command": "echo $$ >\"$OUT/probe\"; sh -c 'echo $$ >\"$OUT/probe-child\"; exec sleep 600' & exec sleep 600" });
    sup.spec(json!({ "a": a }), false);
    sup.start();
    let probe = sup.file_pid("probe");
    let probe_child = sup.file_pid("probe-child");
    sup.spec(json!({}), true);
    sup.until(15, "the supervisor exits", |s| s["supervisor"].is_null());
    assert!(!alive(probe) && !alive(probe_child), "a readiness probe outlived the shutdown");
}

// ---- publishing the state (council item 3) ---------------------------------------------------

#[test]
fn services_are_stopped_when_their_record_cannot_be_published() {
    let sup = Supervisor::new();
    fs::create_dir_all(sup.path().join("state.json")).unwrap();
    sup.spec(json!({ "a": sup.service("echo $$ >\"$OUT/a\"; exec sleep 600") }), false);
    sup.start();
    let a = sup.file_pid("a");
    // Nothing may run that no record names for long.
    assert!(gone_soon(a), "a service ran with no record of it");
    let log = fs::read_to_string(sup.path().join("supervisor.log")).unwrap_or_default();
    let _ = log;
}

#[test]
fn a_record_that_could_not_be_written_is_written_once_it_can_be() {
    let sup = Supervisor::new();
    fs::create_dir_all(sup.path().join("state.json")).unwrap();
    sup.spec(json!({ "a": sup.service("echo $$ >\"$OUT/a\"; exec sleep 600") }), false);
    sup.start();
    let a = sup.file_pid("a");
    fs::remove_dir(sup.path().join("state.json")).unwrap();
    let state = sup.until(3, "the record is published once it can be", |s| s["services"]["a"]["process"]["pid"].as_u64() == Some(u64::from(a)));
    assert_eq!(state["services"]["a"]["status"], "running");
    sup.shutdown();
    assert!(!alive(a));
}

// ---- start requests a running service satisfied (council item 5) ---------------------------

#[test]
fn a_start_asked_of_a_running_service_does_not_restart_it_after_it_dies() {
    let sup = Supervisor::new();
    let mut a = sup.service("echo run >>\"$OUT/runs\"; exec sleep 600");
    sup.spec(json!({ "a": a.clone() }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    // Asked again while it runs (a later `up`): satisfied by the running instance.
    a["start_seq"] = json!(2);
    sup.spec(json!({ "a": a }), false);
    sup.until(5, "applied", |s| s["applied"] == 2);
    let pid = sup.pid("a").unwrap();
    end(pid);
    sup.until(5, "a failed", |s| s["services"]["a"]["status"] == "failed");
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(fs::read_to_string(sup.dir.path().join("runs")).unwrap().lines().count(), 1, "restarted though nothing asked");
    assert_eq!(sup.state()["services"]["a"]["restarts"], 0);
    sup.shutdown();
}

/// A start asked while the service is still stopping is not consumed by the instance being
/// stopped: the service starts again once that stop completed.
#[test]
fn a_start_asked_while_the_service_is_stopping_starts_it_once_the_stop_completed() {
    let sup = Supervisor::new();
    let mut a = sup.service(r#"trap '' TERM; echo run >>"$OUT/runs"; exec "$PY" -c 'import time; time.sleep(600)'"#);
    sup.spec(json!({ "a": a.clone() }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let first = sup.pid("a").unwrap();
    // Once it ignores SIGTERM.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(sup.dir.path().join("runs")).is_ok_and(|r| r.lines().count() == 1) {
        assert!(Instant::now() < deadline, "never ran");
        thread::sleep(Duration::from_millis(10));
    }
    a["want"] = json!(false);
    sup.spec(json!({ "a": a.clone() }), false);
    sup.until(5, "a stopping", |s| s["services"]["a"]["status"] == "stopping");
    thread::sleep(Duration::from_secs(1));
    a["want"] = json!(true);
    a["start_seq"] = json!(2);
    sup.spec(json!({ "a": a }), false);
    sup.until(5, "applied", |s| s["applied"] == 3);
    // It ignores SIGTERM, so the stop ends with the kill after the grace.
    let state = sup.until(20, "a runs again", |s| s["services"]["a"]["status"] == "running" && s["services"]["a"]["process"]["pid"].as_u64() != Some(u64::from(first)));
    assert!(!alive(first), "{state}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(sup.dir.path().join("runs")).is_ok_and(|r| r.lines().count() == 2) {
        assert!(Instant::now() < deadline, "the second run never started");
        thread::sleep(Duration::from_millis(10));
    }
    sup.shutdown();
}

/// The supervisor's own log stays bounded while one supervisor runs, not only when the next one
/// is started: diagnostics it writes past the bound cut it to its tail.
#[test]
fn the_supervisors_own_log_is_cut_while_it_runs() {
    let sup = Supervisor::new();
    let a = sup.service("exec sleep 600");
    sup.spec(json!({ "a": a.clone() }), false);
    sup.start_logged();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    let supervisor = sup.supervisor_pid();
    let log = sup.path().join("supervisor.log");
    // Nearly a full log already (as from a long life), then diagnostics: an unreadable spec,
    // each time it is told to read it.
    let line = "an earlier diagnostic of this supervisor\n";
    fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(line.repeat(1024 * 1024 / line.len()).as_bytes()).unwrap();
    for _ in 0..20 {
        fs::write(sup.path().join("spec.json"), "{ not json").unwrap();
        sup.wake();
        thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::metadata(&log).unwrap().len() > 300 * 1024 {
        assert!(Instant::now() < deadline, "not cut: {} bytes", fs::metadata(&log).unwrap().len());
        thread::sleep(Duration::from_millis(20));
    }
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.contains("keeping the last spec read"), "the diagnostics are kept");
    assert!(text.starts_with(line) || text.starts_with("stack supervisor"), "cut at a line");
    assert_eq!(sup.supervisor_pid(), supervisor, "the same supervisor");
    assert!(alive(supervisor));
    sup.revision.set(1);
    sup.spec(json!({ "a": a }), false);
    sup.shutdown();
}

// ---- acknowledgments (council item 7) ------------------------------------------------------

#[test]
fn an_acknowledgment_comes_only_from_a_supervisor_that_read_the_request() {
    let sup = Supervisor::new();
    sup.spec(json!({ "a": sup.service("exec sleep 600") }), false);
    sup.start();
    sup.until(5, "a runs", |s| s["services"]["a"]["status"] == "running");
    // The spec is lost and rewritten from nothing (revision 0) while the state still says 1;
    // the supervisor is held so it cannot act.
    let mut reset: Value = serde_json::from_slice(&fs::read(sup.path().join("spec.json")).unwrap()).unwrap();
    reset["revision"] = json!(0);
    fs::write(sup.path().join("spec.json"), reset.to_string()).unwrap();
    let supervisor = sup.supervisor_pid();
    unsafe { libc::kill(supervisor as i32, libc::SIGSTOP) };
    let dir = sup.path();
    let request = thread::spawn(move || stack::supervisor::apply(&dir, |spec| spec.services.values_mut().for_each(|s| s.want = false)));
    thread::sleep(Duration::from_millis(500));
    let answered = request.is_finished();
    unsafe { libc::kill(supervisor as i32, libc::SIGCONT) };
    let state = request.join().unwrap().unwrap();
    assert!(!answered, "acknowledged by a supervisor that had not read the request");
    assert!(state.applied >= 2, "{state:?}");
    sup.shutdown();
}

#[test]
fn a_spec_that_exists_but_cannot_be_read_is_an_error_not_a_fresh_start() {
    let sup = Supervisor::new();
    fs::create_dir_all(sup.path()).unwrap();
    fs::write(sup.path().join("spec.json"), "{ not json").unwrap();
    let e = stack::supervisor::apply(&sup.path(), |_| {}).unwrap_err();
    assert_eq!(e.code, "state_invalid", "{e:?}");
    assert_eq!(fs::read_to_string(sup.path().join("spec.json")).unwrap(), "{ not json", "left as it was");
}

// ---- logs ------------------------------------------------------------------------------------

#[test]
fn one_writer_rotates_a_services_log_and_keeps_the_older_file() {
    let sup = Supervisor::new();
    // About 20 MB of output, then quiet.
    sup.spec(json!({ "loud": sup.service(r#""$PY" -c 'import sys; [sys.stdout.write("x" * 1000 + "\n") for _ in range(20000)]; sys.stdout.flush()'; echo done; exec sleep 600"#) }), false);
    sup.start();
    let log = sup.path().join("logs/loud.log");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fs::read_to_string(&log).is_ok_and(|t| t.contains(" done")) {
        assert!(Instant::now() < deadline, "the output never reached the log");
        thread::sleep(Duration::from_millis(50));
    }
    let older = sup.path().join("logs/loud.log.1");
    let (now, before) = (fs::metadata(&log).unwrap().len(), fs::metadata(&older).unwrap().len());
    assert!(before > 16 * 1024 * 1024 && now < before, "{now} {before}");
    // Every line went to one of the two files, none twice.
    let lines = fs::read_to_string(&log).unwrap().lines().count() + fs::read_to_string(&older).unwrap().lines().count();
    assert_eq!(lines, 20001);
    sup.shutdown();
}

/// What a service that logs meets when its supervisor dies: nobody reads its output, so a write
/// fails (`SIGPIPE`, or `EPIPE` where it is ignored); one that does not ignore `SIGPIPE` exits.
/// The next `up` starts it again (see docs/user/worktrees.md).
#[test]
fn a_service_that_writes_after_its_supervisor_died_gets_sigpipe() {
    let sup = Supervisor::new();
    sup.spec(json!({ "talk": sup.service("while :; do echo tick; sleep 0.1; done") }), false);
    sup.start();
    sup.until(5, "talk runs", |s| s["services"]["talk"]["status"] == "running");
    let talk = sup.pid("talk").unwrap();
    sup.kill_supervisor();
    assert!(gone_soon(talk), "it kept running without anyone reading its output");
}

// ---- waiting -------------------------------------------------------------------------------

/// Seconds of CPU `pid` used so far, as `ps` reports it (`M:SS.ss` on macOS, `[[DD-]HH:]MM:SS`
/// on Linux).
fn cpu_seconds(pid: u32) -> f64 {
    let out = Command::new("ps").args(["-o", "time=", "-p", &pid.to_string()]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    text.split(':').fold(0.0, |total, part| total * 60.0 + part.parse::<f64>().unwrap_or(0.0))
}

#[test]
fn a_service_that_never_becomes_ready_costs_the_supervisor_little() {
    let sup = Supervisor::new();
    let mut a = sup.service("exec sleep 600");
    a["ready"] = json!({ "kind": "port", "port": 1 });
    sup.spec(json!({ "a": a }), false);
    sup.start();
    sup.until(5, "a starting", |s| s["services"]["a"]["status"] == "starting");
    let pid = sup.supervisor_pid();
    thread::sleep(Duration::from_secs(3));
    let before = cpu_seconds(pid);
    thread::sleep(Duration::from_secs(5));
    let used = cpu_seconds(pid) - before;
    // Backed off to one check a second: nothing like the 5% of a core of a 10 ms loop.
    assert!(used < 0.1, "{used}s of CPU in 5s of waiting");
    sup.shutdown();
}

#[test]
fn shutdown_drains_final_service_output_before_exiting() {
    let sup = Supervisor::new();
    let script = r#"exec "$PY" -c '
import os, signal, sys, time
def bye(*_):
    for i in range(1, 6):
        sys.stdout.write(f"goodbye-{i}\n")
    sys.stdout.flush()
    os._exit(0)
signal.signal(signal.SIGTERM, bye)
open(os.environ["OUT"] + "/ready", "w").write("ok")
while True:
    time.sleep(10)
'"#;
    sup.spec(json!({ "talk": sup.service(script) }), false);
    sup.start();
    let ready = sup.dir.path().join("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "service never armed its SIGTERM handler");
        thread::sleep(Duration::from_millis(10));
    }
    sup.shutdown();
    let text = fs::read_to_string(sup.path().join("logs/talk.log")).unwrap_or_default();
    for i in 1..=5 {
        assert!(text.contains(&format!("goodbye-{i}")), "missing goodbye-{i} in:\n{text}");
    }
}

#[test]
fn a_detached_descendant_holding_stdout_open_does_not_hang_shutdown() {
    struct Kill(u32);
    impl Drop for Kill {
        fn drop(&mut self) {
            end(self.0);
        }
    }
    let sup = Supervisor::new();
    let script = r#"exec "$PY" -c '
import os, subprocess, sys, time
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
open(os.environ["OUT"] + "/escaped", "w").write(str(child.pid))
time.sleep(600)
'"#;
    sup.spec(json!({ "talk": sup.service(script) }), false);
    sup.start();
    sup.until(5, "talk runs", |s| s["services"]["talk"]["status"] == "running");
    let _escaped = Kill(sup.file_pid("escaped"));
    let started = Instant::now();
    sup.shutdown();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "shutdown hung on detached descendant: {took:?}");
    // A subsequent supervisor starts cleanly without hitting the flock timeout.
    sup.spec(json!({ "after": sup.service("exec sleep 600") }), false);
    sup.start();
    sup.until(5, "after runs", |s| s["services"]["after"]["status"] == "running");
    sup.shutdown();
}
