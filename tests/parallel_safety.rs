#![cfg(unix)]
//! Parallel safety and leases: GC that never waits on another checkout, `gc --dry-run`, the
//! structured lease, and fewer round-trips, against the fake mise of tests/fakes.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

/// The supervisor as stack reaches it without a project, as in tests/runtime.rs: answers from
/// files the test controls, and "stops" a daemon by killing the PID it was told it tracks.
const PITCHFORK: &str = r#"#!/bin/sh
echo "$PITCHFORK_STATE_DIR $*" >>"$REVIEW_FIXTURE/pitchfork.log"
# `pf-supervisor`, when it exists, is a running supervisor: the PID it holds, if any, is its
# process, which `supervisor stop` ends.
case "$1" in
  status)
    if test "$3" = global/pitchfork; then
      pid=$(cat "$REVIEW_FIXTURE/pf-supervisor" 2>/dev/null)
      if test -n "$pid"; then echo "{\"id\":\"global/pitchfork\",\"status\":\"running\",\"pid\":$pid}"; exit 0; fi
      echo "Error: Daemon $3 not found" >&2; exit 1
    fi
    # `pf-status/<id>.json`, when a test gives one, is what Pitchfork says about that daemon.
    if test -f "$REVIEW_FIXTURE/pf-status/$3.json"; then cat "$REVIEW_FIXTURE/pf-status/$3.json"; exit 0; fi
    if test -f "$REVIEW_FIXTURE/pf-status.json"; then cat "$REVIEW_FIXTURE/pf-status.json"; exit 0; fi
    echo "Error: Daemon $3 not found" >&2; exit 1 ;;
  list)
    # With `pf-list-gate`, the next list is answered only once `release-list` exists (at most
    # 30 seconds), after `list-ready` says it was asked: a caller held right after it looked.
    if mv "$REVIEW_FIXTURE/pf-list-gate" "$REVIEW_FIXTURE/pf-list-gated" 2>/dev/null; then
      touch "$REVIEW_FIXTURE/list-ready"; n=0
      while ! test -f "$REVIEW_FIXTURE/release-list" && test $n -lt 600; do sleep 0.05; n=$((n + 1)); done
    fi
    if test -f "$REVIEW_FIXTURE/pf-list.json"; then cat "$REVIEW_FIXTURE/pf-list.json"; else echo '[]'; fi ;;
  supervisor)
    case "$2" in
      status) test -f "$REVIEW_FIXTURE/pf-supervisor" ;;
      stop)
        if test -f "$REVIEW_FIXTURE/pf-fail-supervisor-stop"; then echo 'cannot stop the supervisor' >&2; exit 1; fi
        pid=$(cat "$REVIEW_FIXTURE/pf-supervisor" 2>/dev/null); test -n "$pid" && kill "$pid"
        rm -f "$REVIEW_FIXTURE/pf-supervisor"
        # With `supervisor-owns-services`, a supervisor takes the services it runs with it: those
        # a request that reached this state directory started (see the fake mise).
        if test -f "$REVIEW_FIXTURE/supervisor-owns-services" && test -f "$REVIEW_FIXTURE/supervised-by.log"; then
          while read -r state service; do
            if test "$state" = "$PITCHFORK_STATE_DIR"; then kill "$service" 2>/dev/null; fi
          done <"$REVIEW_FIXTURE/supervised-by.log"
        fi
        true ;;
    esac ;;
  stop)
    if test -f "$REVIEW_FIXTURE/pf-fail-stop"; then echo 'cannot stop' >&2; exit 1; fi
    kill "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")" ;;
esac
"#;

struct Fixture {
    dir: TempDir,
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
        write_exe(&dir.path().join("bin/mise"), FAKE_MISE);
        write_exe(&dir.path().join("bin/pitchfork"), PITCHFORK);
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("PATH", format!("{}:{}", self.path("bin").display(), std::env::var("PATH").unwrap()))
            .env("REVIEW_FIXTURE", self.dir.path())
            .env("REVIEW_PYTHON", python::python())
            .env("STACK_STATE_DIR", self.path("state"))
            .env("STACK_CACHE_DIR", self.path("cache"))
            .env("PITCHFORK_STATE_DIR", self.path("pfs"));
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

    fn json(&self, args: &[&str]) -> Value {
        json_result(&self.command(args).output().unwrap())
    }

    fn index_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path("state/sessions")).map_or(Vec::new(), |d| d.map(|e| e.unwrap().path()).collect())
    }

    /// Tool calls to one `stack mcp`, each given this fixture's app as `dir` (unless it names
    /// one); the `structuredContent` of each.
    fn mcp(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut input = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string();
        input.push('\n');
        for (i, (name, args)) in calls.iter().enumerate() {
            let mut args = args.clone();
            if args.get("dir").is_none() {
                args["dir"] = json!(self.path("app"));
            }
            input.push_str(&format!("{}\n", json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":name,"arguments":args}})));
        }
        let mut child = self.command(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "MCP server failed: {}", String::from_utf8_lossy(&out.stderr));
        let responses: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        (1..=calls.len()).map(|id| responses.iter().find(|r| r["id"] == id).unwrap()["result"]["structuredContent"].clone()).collect()
    }

    /// Rewrite every copy of the session record.
    fn edit_session(&self, edit: impl Fn(&mut Value)) {
        let mut paths = self.index_files();
        paths.push(self.path("app/.stack/session.json"));
        for path in paths.into_iter().filter(|p| p.exists()) {
            let mut session: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            edit(&mut session);
            fs::write(&path, session.to_string()).unwrap();
        }
    }

    /// Machine-wide gc, run from outside any project.
    fn gc(&self, extra: &[&str]) -> (bool, Value) {
        let mut args = vec!["gc", "--json"];
        args.extend_from_slice(extra);
        let out = self.command_at(self.dir.path(), &args).output().unwrap();
        (out.status.success(), serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
    }
}

fn write_exe(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn json_result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn gc_step(up: &Value) -> Value {
    up["data"]["steps"].as_array().unwrap().iter().find(|s| s["step"] == "gc").cloned().unwrap()
}

// ---- gc inside up ------------------------------------------------------------------------

#[test]
fn up_skips_another_checkouts_busy_expired_session_instead_of_waiting_for_it() {
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    fixture.ok_at(&other, &["up", "--ttl", "1s"]);
    thread::sleep(Duration::from_millis(2100));
    // Another command (a long `up`, say) holds the other checkout's lock throughout.
    let held = stack::state::project_lock(&fixture.path("state"), &other.canonicalize().unwrap()).unwrap();

    let started = Instant::now();
    let up = fixture.json(&["up", "--json", "--timeout", "20s"]);
    assert_eq!(up["ok"], true, "{up}");
    assert!(started.elapsed() < Duration::from_secs(10), "up waited for another checkout: {:?}", started.elapsed());
    let gc = gc_step(&up);
    assert_eq!(gc["status"], "ok", "{gc}");
    assert_eq!(gc["detail"]["skipped"][0]["project"], json!(other.canonicalize().unwrap()), "{gc}");
    assert_eq!(gc["detail"]["skipped"][0]["skipped"], "busy");
    assert!(gc["detail"]["skipped"][0]["reason"].as_str().unwrap().contains("lease expired"), "{gc}");
    assert_eq!(fixture.index_files().len(), 2, "the busy session was not touched");

    drop(held);
    let up = fixture.json(&["up", "--json"]);
    assert_eq!(up["data"]["reaped"][0]["project"], json!(other.canonicalize().unwrap()), "{up}");
    assert!(gc_step(&up).get("detail").is_none(), "a routine gc step stays compact: {up}");
    assert_eq!(fixture.index_files().len(), 1);
}

#[test]
fn a_gc_problem_is_a_warning_of_up_never_its_failure() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.path("state/sessions")).unwrap();
    fs::write(fixture.path("state/sessions/broken.json"), "not json").unwrap();

    let up = fixture.json(&["up", "--json"]);
    assert_eq!(up["ok"], true, "{up}");
    let gc = gc_step(&up);
    assert_eq!(gc["status"], "warning", "{gc}");
    assert!(gc["detail"]["warnings"][0].as_str().unwrap().contains("broken.json"), "{gc}");
    assert!(up["data"]["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("gc: ")), "{up}");
    // The standalone collector still reports it.
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok && result["error"]["code"] == "state_invalid", "{result}");
}

#[test]
fn standalone_gc_never_locks_a_session_that_is_not_a_candidate() {
    let fixture = Fixture::new();
    let other = fixture.add_project("other");
    fixture.ok_at(&other, &["up"]);
    let held = stack::state::project_lock(&fixture.path("state"), &other.canonicalize().unwrap()).unwrap();
    let mut child = fixture.command_at(fixture.dir.path(), &["gc", "--json"]).stdout(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "gc waited for the lock of a session it had no reason to reclaim");
        thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(json_result(&out)["data"], json!([]));
    drop(held);
}

// ---- gc --dry-run ------------------------------------------------------------------------

#[test]
fn gc_dry_run_reports_what_would_be_reclaimed_and_changes_nothing() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "1s"]);
    fixture.edit_session(|s| s["services"] = json!({ "worker": { "port": 41234, "pid": 999_999, "identity": "liveness", "verified_at": 0, "provider_id": "app-x/worker" } }));
    // A deleted checkout with no session left: a port reservation and a data directory.
    let gone = fixture.add_project("gone");
    let gone = gone.canonicalize().unwrap();
    let mut ports: Value = serde_json::from_slice(&fs::read(fixture.path("state/ports.json")).unwrap()).unwrap();
    ports["reservations"].as_array_mut().unwrap().push(json!({ "port": 41999, "project": gone, "service": "db" }));
    fs::write(fixture.path("state/ports.json"), ports.to_string()).unwrap();
    let data_dir = fixture.path("mise-state/daemons/x/data/db");
    fs::write(fixture.path("state/data.json"), json!({ "entries": [{ "project": gone, "service": "db", "dir": data_dir, "checkout": "token" }] }).to_string()).unwrap();
    fs::remove_dir_all(&gone).unwrap();
    thread::sleep(Duration::from_millis(2100));
    let state_files = || -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<PathBuf> = fixture.index_files();
        files.extend([fixture.path("state/ports.json"), fixture.path("state/data.json"), fixture.path("app/.stack/session.json")]);
        files.into_iter().map(|p| (p.clone(), fs::read(&p).unwrap())).collect()
    };
    let before = state_files();

    let (ok, result) = fixture.gc(&["--dry-run"]);
    assert!(ok, "{result}");
    let data = &result["data"];
    assert_eq!(data["dry_run"], true);
    let planned = &data["would_reclaim"][0];
    assert_eq!(planned["project"], json!(fixture.path("app").canonicalize().unwrap()));
    assert_eq!(planned["action"], "stop");
    assert!(planned["reason"].as_str().unwrap().contains("lease expired"), "{planned}");
    assert!(planned["session"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(planned["services"][0], json!({ "service": "worker", "port": 41234, "pid": 999_999, "pid_alive": false, "provider_id": "app-x/worker" }));
    let deleted = data["deleted_checkouts"].as_array().unwrap().iter().find(|d| d["project"] == json!(gone)).cloned().unwrap_or_else(|| panic!("{data}"));
    assert!(deleted["ports"].as_array().unwrap().contains(&json!({ "service": "db", "port": 41999 })), "{deleted}");
    assert_eq!(deleted["data"], json!([{ "service": "db", "dir": data_dir, "action": "report" }]));
    assert!(deleted.get("after_session").is_none(), "{deleted}");
    // With --reclaim-data, what it would remove.
    let (ok, result) = fixture.gc(&["--dry-run", "--reclaim-data"]);
    assert!(ok, "{result}");
    let deleted = result["data"]["deleted_checkouts"].as_array().unwrap().iter().find(|d| d["project"] == json!(gone)).cloned().unwrap();
    assert_eq!(deleted["data"][0]["action"], "remove_if_safe", "{deleted}");
    assert_eq!(before, state_files(), "a dry run changed a record");
    let log = fs::read_to_string(fixture.path("mise.log")).unwrap_or_default();
    assert!(!log.contains("daemons stop"), "{log}");
    assert!(!fixture.path("pitchfork.log").exists(), "a dry run asked the supervisor");

    let out = fixture.command_at(fixture.dir.path(), &["gc", "--dry-run"]).output().unwrap();
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(human.contains("would stop") && human.contains("port 41999 would be released"), "{human}");
    let (ok, result) = fixture.gc(&["--dry-run", "--watch"]);
    assert!(!ok && result["error"]["code"] == "usage", "{result}");
    let results = fixture.mcp(&[("stack_gc", json!({ "dry_run": true, "dir": null }))]);
    assert_eq!(results[0]["data"]["would_reclaim"][0]["action"], "stop", "{}", results[0]);
    assert_eq!(before, state_files(), "a dry run over MCP changed a record");
}

// ---- gc --stop-orphans -------------------------------------------------------------------

const WEB: &str = "[bundle]\nname='test'\n[services.web]\nrun='true'\n";
const WEB_ID: &str = "app-0123456789abcdef/web";

/// A detached process standing in for a supervised service, killed when dropped.
struct Detached(u32);

impl Detached {
    fn spawn() -> Self {
        let out = Command::new("sh").args(["-c", "sleep 300 >/dev/null 2>&1 & echo $!"]).output().unwrap();
        Self(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap())
    }

    fn alive(&self) -> bool {
        Command::new("kill").args(["-0", &self.0.to_string()]).stderr(Stdio::null()).status().unwrap().success()
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.0.to_string()).stderr(Stdio::null()).status();
    }
}

struct Running {
    service: Detached,
    port: u16,
    listener: Option<std::net::TcpListener>,
}

impl Fixture {
    /// `stack up` of one supervised service that is really running and listening.
    fn start_web(&self) -> Running {
        let inspect = json_result(&self.ok(&["inspect", "--json"]));
        let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
        let service = Detached::spawn();
        fs::write(
            self.path("daemons-started.json"),
            json!([{ "id": WEB_ID, "name": "web", "status": "running", "pid": service.0, "port": port }]).to_string(),
        )
        .unwrap();
        let up = self.command(&["up", "--json"]).stdout(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.path("started").exists() {
            assert!(Instant::now() < deadline, "provider start never ran");
            thread::sleep(Duration::from_millis(10));
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let out = up.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        Running { service, port, listener: Some(listener) }
    }

    /// What the supervisor says it tracks under the recorded id, and what `stop` kills.
    fn supervisor_tracks(&self, pid: u32, port: u16) {
        fs::write(self.path("pf-status.json"), json!({ "id": WEB_ID, "status": "running", "pid": pid, "active_port": port }).to_string()).unwrap();
        fs::write(self.path("pf-tracked-pid"), pid.to_string()).unwrap();
    }

    fn pitchfork_log(&self) -> String {
        fs::read_to_string(self.path("pitchfork.log")).unwrap_or_default()
    }

    /// Rewrite the index record of a checkout whose directory is gone.
    fn edit_index(&self, edit: impl Fn(&mut Value)) {
        for path in self.index_files() {
            let mut session: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            edit(&mut session);
            fs::write(&path, session.to_string()).unwrap();
        }
    }
}

#[test]
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
fn stop_orphans_stops_a_deleted_checkouts_service_only_when_every_recorded_fact_matches() {
    let fixture = Fixture::with_bundle(WEB);
    let mut running = fixture.start_web();
    let pid = running.service.0;
    let started = stack::procinfo::process(pid).and_then(|p| p.started_unix_ms()).expect("start time readable");
    fixture.supervisor_tracks(pid, running.port);
    fs::remove_dir_all(fixture.path("app")).unwrap();

    // Without the flag nothing changes, and the hint names it.
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok && result["error"]["code"] == "gc_incomplete", "{result}");
    assert!(result["error"]["hint"].as_str().unwrap().contains("stack gc --stop-orphans"), "{result}");
    assert!(running.service.alive());

    for (case, recorded) in [("an hour later", Some(started + 3_600_000)), ("an hour earlier", Some(started - 3_600_000)), ("unrecorded", None)] {
        fixture.edit_index(|s| s["services"]["web"]["started_at_ms"] = json!(recorded));
        let (ok, result) = fixture.gc(&["--stop-orphans"]);
        assert!(!ok && result["error"]["code"] == "gc_incomplete", "{case}: {result}");
        let error = result["error"]["details"][0]["services"][0]["error"].as_str().unwrap();
        assert!(error.contains("--stop-orphans") && error.contains("not signalled"), "{case}: {error}");
        assert!(result["error"]["details"][0]["services"][0]["recovery"]["stop"].is_string(), "{case}: {result}");
        assert!(running.service.alive(), "{case}: signalled");
        assert!(!fixture.pitchfork_log().contains(" stop "), "{case}");
    }

    fixture.edit_index(|s| s["services"]["web"]["started_at_ms"] = json!(started));
    // The service's port closes with it.
    drop(running.listener.take());
    let (ok, result) = fixture.gc(&["--stop-orphans"]);
    assert!(ok, "{result}");
    let outcome = result["data"][0]["services"][0]["outcome"].as_str().unwrap();
    assert!(outcome.contains("stopped by --stop-orphans"), "{outcome}");
    assert!(!running.service.alive(), "the matching service was not stopped");
    assert!(fixture.index_files().is_empty(), "record kept after a confirmed stop");
    assert!(fixture.pitchfork_log().lines().any(|l| l.ends_with(&format!("stop -- {WEB_ID}"))), "{}", fixture.pitchfork_log());
}

#[test]
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
fn stop_orphans_never_stops_a_service_whose_supervisor_reports_another_pid() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web();
    let other = Detached::spawn();
    fixture.supervisor_tracks(other.0, running.port);
    fs::remove_dir_all(fixture.path("app")).unwrap();
    let (ok, result) = fixture.gc(&["--stop-orphans"]);
    assert!(!ok && result["error"]["code"] == "gc_incomplete", "{result}");
    assert!(running.service.alive() && other.alive());
    assert!(!fixture.pitchfork_log().contains(" stop "));
    let (ok, result) = fixture.gc(&["--dry-run", "--stop-orphans"]);
    assert!(!ok && result["error"]["code"] == "usage", "{result}");
    let results = fixture.mcp(&[("stack_gc", json!({ "dry_run": true, "stop_orphans": true, "dir": null }))]);
    assert_eq!(results[0]["error"]["code"], "usage", "{}", results[0]);
}

// ---- leases ------------------------------------------------------------------------------

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[test]
fn status_renews_a_lease_and_status_and_up_report_it_structured() {
    let fixture = Fixture::new();
    let up = fixture.json(&["up", "--json", "--ttl", "30m"]);
    let lease = &up["data"]["lease"];
    assert_eq!((lease["kind"].as_str(), lease["ttl_secs"].as_u64(), lease["expired"].as_bool()), (Some("ttl"), Some(1800), Some(false)), "{up}");
    assert_eq!(lease["expires_at"].as_u64(), Some(lease["renewed_at"].as_u64().unwrap() + 1800), "{up}");

    // Make the lease look idle for ten minutes, then check: status counts as activity.
    let idle_since = unix_now() - 600;
    fixture.edit_session(|s| s["lease"]["renewed_at"] = json!(idle_since));
    let status = fixture.json(&["status", "--json"]);
    assert_eq!(status["data"]["lease"]["kind"], "ttl", "{status}");
    let renewed = status["data"]["lease"]["renewed_at"].as_u64().unwrap();
    assert!(renewed >= idle_since + 600, "status did not renew: {status}");
    assert_eq!(status["data"]["session"]["lease"]["renewed_at"], renewed);
    let saved: Value = serde_json::from_slice(&fs::read(fixture.path("app/.stack/session.json")).unwrap()).unwrap();
    assert_eq!(saved["lease"]["renewed_at"], renewed);
    let human = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
    assert!(human.contains("lease: ttl 1800s, "), "{human}");

    // An expired lease is reported, not revived.
    fixture.edit_session(|s| {
        s["lease"]["ttl_secs"] = json!(1);
        s["lease"]["renewed_at"] = json!(1_000);
    });
    let status = fixture.json(&["status", "--json"]);
    assert_eq!(status["data"]["lease"]["expired"], true, "{status}");
    assert_eq!(status["data"]["lease"]["expires_at"], 1_001);
    assert!(status["data"]["lease"]["expired_reason"].as_str().unwrap().contains("lease expired"), "{status}");
    assert!(status["data"]["lease_expired"].is_string());
    assert_eq!(status["data"]["session"]["lease"]["renewed_at"], 1_000);
    let human = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
    assert!(human.contains("lease: lease expired"), "{human}");

    // No lease: kind none.
    fixture.ok(&["down"]);
    fixture.ok(&["up"]);
    let status = fixture.json(&["status", "--json"]);
    assert_eq!(status["data"]["lease"], json!({ "kind": "none", "renewed_at": 0, "expired": false }), "{status}");
    let results = fixture.mcp(&[("stack_status", json!({}))]);
    assert_eq!(results[0]["data"]["lease"]["kind"], "none", "{}", results[0]);
}

#[test]
fn renew_sets_the_ttl_and_a_session_without_a_lease_says_how_to_give_it_one() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let out = fixture.command(&["renew", "--json"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "no_lease");
    assert!(error["hint"].as_str().unwrap().contains("stack renew --ttl"), "{error}");

    let renewed = json_result(&fixture.ok(&["renew", "--json", "--ttl", "45m"]));
    assert_eq!(renewed["data"]["lease"]["ttl_secs"], 2700, "{renewed}");
    let renewed = json_result(&fixture.ok(&["renew", "--json", "--ttl", "10m"]));
    assert_eq!(renewed["data"]["lease"]["ttl_secs"], 600, "{renewed}");
    let results = fixture.mcp(&[("stack_renew", json!({ "ttl": "2h" })), ("stack_status", json!({}))]);
    assert_eq!(results[0]["data"]["lease"]["ttl_secs"], 7200, "{}", results[0]);
    assert_eq!(results[1]["data"]["lease"]["ttl_secs"], 7200, "{}", results[1]);
    let results = fixture.mcp(&[("stack_renew", json!({ "ttl": "soon" }))]);
    assert_eq!(results[0]["error"]["code"], "usage", "{}", results[0]);
}

#[test]
fn owner_pid_can_name_the_nearest_ancestor_by_command_name() {
    let fixture = Fixture::new();
    // This test process is the parent of the stack command it runs.
    let me = stack::procinfo::named(std::process::id()).unwrap();
    let up = fixture.json(&["up", "--json", "--owner-pid", &format!("ancestor:{}", me.comm)]);
    assert_eq!(up["data"]["session"]["lease"]["owner_pid"], std::process::id(), "{up}");
    assert_eq!(up["data"]["lease"]["kind"], "owner_pid");
    assert_eq!(up["data"]["lease"]["owner_alive"], true);
    fixture.ok(&["down"]);

    let log = fixture.path("mise.log");
    let calls = fs::read_to_string(&log).unwrap_or_default().lines().count();
    let out = fixture.command(&["up", "--json", "--owner-pid", "ancestor:no-such-runner"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "owner_not_found", "{error}");
    assert!(error["message"].as_str().unwrap().contains(&format!("{} {}", std::process::id(), me.comm)), "{error}");
    assert!(error["hint"].as_str().unwrap().contains("--ttl"), "{error}");
    assert_eq!(fs::read_to_string(&log).unwrap_or_default().lines().count(), calls, "lifecycle work ran");
    assert!(!fixture.path("app/.stack/session.json").exists());

    let out = fixture.command(&["up", "--json", "--owner-pid", "abc"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_result(&out)["error"]["code"], "usage");

    // Over MCP the server itself is the stack process; its parent is this test.
    let results = fixture.mcp(&[
        ("stack_up", json!({ "owner_ancestor": me.comm })),
        ("stack_up", json!({ "owner_ancestor": "x", "owner_pid": 5 })),
    ]);
    assert_eq!(results[0]["data"]["session"]["lease"]["owner_pid"], std::process::id(), "{}", results[0]);
    assert_eq!(results[1]["error"]["code"], "usage", "{}", results[1]);
    fixture.ok(&["down"]);
}

// ---- fewer round-trips -------------------------------------------------------------------

fn saved_session(fixture: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(fixture.path("app/.stack/session.json")).unwrap()).unwrap()
}

#[test]
fn exec_up_starts_the_stack_only_when_needed_and_keeps_the_lease() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.t]\nrun='true'\n");
    let result = json_result(&fixture.ok(&["--json", "exec", "--up", "--ttl", "5m", "--", "true"]));
    assert_eq!(result["data"]["exit_code"], 0);
    assert_eq!(result["data"]["prepared"][0]["step"], "up", "{result}");
    assert!(result["data"]["prepared"][0]["reason"].as_str().unwrap().contains("no session"), "{result}");
    assert_eq!(result["data"]["prepared"][0]["lease"]["ttl_secs"], 300, "{result}");
    let session = saved_session(&fixture);
    assert_eq!(session["lease"]["ttl_secs"], 300);
    let id = session["id"].clone();

    // A healthy session: nothing more runs.
    let result = json_result(&fixture.ok(&["--json", "exec", "--up", "--", "true"]));
    assert!(result["data"].get("prepared").is_none(), "{result}");

    // A configuration change makes the session stale: up runs again, keeping the TTL.
    fs::write(fixture.path("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[env]\nMODE='changed'\n").unwrap();
    let result = json_result(&fixture.ok(&["--json", "run", "--up", "t"]));
    assert_eq!(result["data"]["prepared"][0]["step"], "up", "{result}");
    assert!(result["data"]["prepared"][0]["reason"].as_str().unwrap().contains("configuration changed"), "{result}");
    let session = saved_session(&fixture);
    assert_ne!(session["id"], id);
    assert_eq!(session["lease"]["ttl_secs"], 300, "{session}");
    let out = fixture.command(&["exec", "--up", "--", "true"]).output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));

    // --ttl alone is refused, on both surfaces.
    let out = fixture.command(&["--json", "exec", "--ttl", "5m", "--", "true"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "usage");
    let results = fixture.mcp(&[
        ("stack_exec", json!({ "command": ["true"], "ttl": "5m" })),
        ("stack_run", json!({ "task": "t", "ttl": "5m" })),
        ("stack_exec", json!({ "command": ["true"], "up": true })),
    ]);
    assert_eq!(results[0]["error"]["code"], "usage", "{}", results[0]);
    assert!(results[0]["error"]["message"].as_str().unwrap().contains("`ttl` needs `up: true`"), "{}", results[0]);
    assert_eq!(results[1]["error"]["code"], "usage", "{}", results[1]);
    assert_eq!(results[2]["data"]["exit_code"], 0, "{}", results[2]);
    assert!(results[2]["data"].get("prepared").is_none(), "{}", results[2]);

    // Stopped: up runs again over MCP too.
    fixture.ok(&["down"]);
    let results = fixture.mcp(&[("stack_run", json!({ "task": "t", "up": true, "ttl": "1h" }))]);
    assert_eq!(results[0]["data"]["prepared"][0]["step"], "up", "{}", results[0]);
    assert_eq!(saved_session(&fixture)["lease"]["ttl_secs"], 3600);
    fixture.ok(&["down"]);
}

#[test]
fn exec_install_installs_missing_pins_instead_of_failing() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.8.2'\n");
    let out = fixture.command(&["--json", "exec", "--", "true"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "tools_not_installed");
    assert!(error["hint"].as_str().unwrap().contains("--install"), "{error}");

    let result = json_result(&fixture.ok(&["--json", "exec", "--install", "--", "true"]));
    assert_eq!(result["data"]["exit_code"], 0, "{result}");
    assert_eq!(result["data"]["prepared"][0]["step"], "install", "{result}");
    assert_eq!(result["data"]["prepared"][0]["missing"], json!(["jq@1.8.2"]));
    // Installed now: the flag does nothing more.
    let result = json_result(&fixture.ok(&["--json", "exec", "--install", "--", "true"]));
    assert!(result["data"].get("prepared").is_none(), "{result}");
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["true"], "install": true })), ("stack_run", json!({ "task": "t", "install": true }))]);
    assert_eq!(results[0]["data"]["exit_code"], 0, "{}", results[0]);
    assert_eq!(results[1]["error"]["code"], "usage", "stack_run installs a task's pins itself: {}", results[1]);
}

#[test]
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
fn up_reassign_ports_moves_a_port_another_program_holds_and_leaves_the_program_alone() {
    let fixture = Fixture::with_bundle(WEB);
    // A provider that starts a supervised listener on whatever port web is assigned when it
    // starts, as a real daemon would.
    let real = fixture.path("bin/mise-fake");
    fs::rename(fixture.path("bin/mise"), &real).unwrap();
    write_exe(
        &fixture.path("bin/mise"),
        &format!(
            r#"#!/bin/sh
if test "$1 $2" = "daemons start"; then
  "$REVIEW_PYTHON" -c '
import json, os, sys
f = sys.argv[1]
r = json.load(open(os.path.join(os.environ["STACK_STATE_DIR"], "ports.json")))
p = [x["port"] for x in r["reservations"] if x["service"] == "web"][0]
open(os.path.join(f, "listen-port"), "w").write(str(p))
json.dump([{{"id": "{WEB_ID}", "name": "web", "status": "running", "port": p}}], open(os.path.join(f, "daemons-started.json"), "w"))
' "$REVIEW_FIXTURE"
fi
exec "{}" "$@"
"#,
            real.display()
        ),
    );
    let inspect = json_result(&fixture.ok(&["inspect", "--json"]));
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
    let squatter = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();

    // Without the flag: the conflict, whose hint names it.
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "port_conflict", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("stack up --reassign-ports"), "{error}");

    let up = json_result(&fixture.ok(&["up", "--json", "--reassign-ports"]));
    let moved = &up["data"]["reassigned_ports"][0];
    assert_eq!((moved["service"].as_str(), moved["old"].as_u64()), (Some("web"), Some(u64::from(port))), "{up}");
    let new = moved["new"].as_u64().unwrap();
    assert_ne!(new, u64::from(port));
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    assert_eq!(up["data"]["checks"][0]["port"], new);
    // The other program still has its port.
    assert!(squatter.local_addr().is_ok());
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
    assert_eq!(json_result(&fixture.ok(&["inspect", "--json"]))["data"]["ports"]["web"], new);
    // Nothing to move: the flag changes nothing.
    let results = fixture.mcp(&[("stack_up", json!({ "reassign_ports": true }))]);
    assert!(results[0]["data"].get("reassigned_ports").is_none(), "{}", results[0]);
    fixture.ok(&["down"]);
}
