#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bin", "bundle", "app"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(
            dir.path().join("bundle/bundle.toml"),
            "[bundle]\nname='test'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("app/stack.toml"),
            "[[use]]\nbundle='path:../bundle'\n",
        )
        .unwrap();
        fs::write(dir.path().join("daemons.json"), "[]").unwrap();
        let path = format!(
            "{}:{}",
            dir.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        fs::write(
            dir.path().join("env.json"),
            json!({"PATH":path}).to_string(),
        )
        .unwrap();
        let mise = dir.path().join("bin/mise");
        fs::write(
            &mise,
            r#"#!/bin/sh
case "$1 $2" in
  'env --json') cat "$REVIEW_FIXTURE/env.json" ;;
  'daemons --json')
    if test -f "$REVIEW_FIXTURE/fail-query"; then echo 'supervisor unavailable' >&2; exit 1; fi
    cat "$REVIEW_FIXTURE/daemons.json" ;;
  'daemons stop')
    if test -f "$REVIEW_FIXTURE/fail-stop"; then echo 'cannot stop' >&2; exit 1; fi ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(mise, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self { dir };
        fixture.ok(&["compile"]);
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .args(["-C", self.dir.path().join("app").to_str().unwrap()])
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("REVIEW_FIXTURE", self.dir.path())
            .env("STACK_STATE_DIR", self.dir.path().join("state"))
            .env("STACK_CACHE_DIR", self.dir.path().join("cache"));
        command
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn session_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths = vec![self.dir.path().join("app/.stack/session.json")];
        paths.extend(
            fs::read_dir(self.dir.path().join("state/sessions"))
                .unwrap()
                .map(|e| e.unwrap().path()),
        );
        paths
    }
}

#[test]
fn cli_exec_runs_in_the_selected_project_directory() {
    let fixture = Fixture::new();
    let out = fixture.ok(&["exec", "--", "pwd"]);
    let actual = Path::new(String::from_utf8_lossy(&out.stdout).trim())
        .canonicalize()
        .unwrap();
    assert_eq!(
        actual,
        fixture.dir.path().join("app").canonicalize().unwrap()
    );
}

#[test]
fn project_overrides_change_the_session_generation_even_when_bundle_pins_do_not() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let lock = fs::read(fixture.dir.path().join("app/stack.lock")).unwrap();
    fs::write(
        fixture.dir.path().join("app/stack.toml"),
        "[[use]]\nbundle='path:../bundle'\n[env]\nMODE='changed'\n",
    )
    .unwrap();
    fixture.ok(&["compile"]);
    assert_eq!(
        fs::read(fixture.dir.path().join("app/stack.lock")).unwrap(),
        lock
    );
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["data"]["stale"], true);
    fixture.ok(&["up"]);
    let out = fixture.ok(&["status", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"]["stale"],
        false
    );
}

#[test]
fn supervisor_failure_preserves_ownership_instead_of_confirming_cleanup() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let paths = fixture.session_paths();
    let mut session: Value = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
    session["services"] = json!({ "worker": { "port":0, "pid":std::process::id(), "identity":"liveness", "verified_at":0 } });
    for path in &paths {
        fs::write(path, session.to_string()).unwrap();
    }
    fs::write(fixture.dir.path().join("fail-query"), "").unwrap();
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "provider_failed"
    );
    for path in paths {
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
            session
        );
    }
}

#[test]
fn recorded_ownership_is_used_even_when_the_supervisor_lists_no_daemons() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let paths = fixture.session_paths();
    let mut session: Value = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
    session["services"] = json!({ "worker": { "port":0, "pid":std::process::id(), "identity":"liveness", "verified_at":0 } });
    for path in &paths {
        fs::write(path, session.to_string()).unwrap();
    }
    fs::write(fixture.dir.path().join("fail-stop"), "").unwrap();
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "stop_failed"
    );
    assert!(paths.iter().all(|p| p.exists()));
}

#[test]
fn concurrent_renewals_keep_both_records_valid_and_consistent() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "10m"]);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..32)
            .map(|_| scope.spawn(|| fixture.ok(&["renew", "--json"])))
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    let records: Vec<Value> = fixture
        .session_paths()
        .iter()
        .map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap())
        .collect();
    assert_eq!(records[0], records[1]);
}

#[test]
fn active_exec_protects_ttl_until_completion_then_the_session_can_expire() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "1s"]);
    let mut child = fixture
        .command(&["exec", "--", "sleep", "3"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let session_file = fixture.dir.path().join("app/.stack/session.json");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let session: Value = serde_json::from_slice(&fs::read(&session_file).unwrap()).unwrap();
        if session["active_executions"]
            .as_object()
            .is_some_and(|v| !v.is_empty())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "execution lease was never registered"
        );
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_secs(2));
    let out = fixture.ok(&["gc", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"],
        json!([])
    );
    assert!(session_file.exists());
    assert!(child.try_wait().unwrap().is_none());
    assert!(child.wait().unwrap().success());
    let session: Value = serde_json::from_slice(&fs::read(&session_file).unwrap()).unwrap();
    assert_eq!(session["active_executions"], json!({}));
    thread::sleep(Duration::from_secs(2));
    let out = fixture.ok(&["gc", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"][0]["stopped"],
        true
    );
    assert!(!session_file.exists());
}
