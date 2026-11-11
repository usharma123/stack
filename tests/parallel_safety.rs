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
