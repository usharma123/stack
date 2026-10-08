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
        Self::with_bundle("[bundle]\nname='test'\n")
    }

    fn with_bundle(bundle: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bin", "bundle", "app"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("bundle/bundle.toml"), bundle).unwrap();
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
echo "$*" >>"$REVIEW_FIXTURE/mise.log"
case "$1 $2" in
  'latest '*)
    if test -f "$REVIEW_FIXTURE/latest-empty"; then exit 0; fi
    v=${2#*@}; if test "$v" = "$2"; then v=1.0.0; fi
    case "$2" in python@3.13) v=3.13.16 ;; postgres@17) v=17.11 ;; redis@8) v=8.2.1 ;; esac
    echo "$v" ;;
  'which pitchfork') echo "$REVIEW_FIXTURE/bin/pitchfork" ;;
  'env --json')
    if test -f "$REVIEW_FIXTURE/fail-env-after-start" && test -f "$REVIEW_FIXTURE/started"; then exit 1; fi
    cat "$REVIEW_FIXTURE/env.json" ;;
  'daemons --json')
    if test -f "$REVIEW_FIXTURE/fail-query-after-one" && test -f "$REVIEW_FIXTURE/started"; then
      if test -f "$REVIEW_FIXTURE/query-observed"; then exit 1; fi
      touch "$REVIEW_FIXTURE/query-observed"
    fi
    if test -f "$REVIEW_FIXTURE/fail-query"; then echo 'supervisor unavailable' >&2; exit 1; fi
    # A supervised listener (see `daemons start`) is reported with its real PID while alive.
    if test -f "$REVIEW_FIXTURE/listen-port" && kill -0 "$(cat "$REVIEW_FIXTURE/pf-tracked-pid" 2>/dev/null)" 2>/dev/null; then
      python3 -c 'import json,sys; pid=int(sys.argv[2]); d=json.load(open(sys.argv[1])); [e.__setitem__("pid", pid) for e in d if e.get("status") in ("running", "starting")]; print(json.dumps(d))' "$REVIEW_FIXTURE/daemons.json" "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")"
    else
      cat "$REVIEW_FIXTURE/daemons.json"
    fi ;;
  'daemons start')
    # A request client that would launch a service later, after the request gave up. Like a
    # real client it keeps SIGINT's default action (a plain `&` job of sh would ignore it).
    if test -f "$REVIEW_FIXTURE/late-client"; then
      python3 -c 'import signal,sys,time; signal.signal(signal.SIGINT, signal.SIG_DFL); open(sys.argv[2], "w").close(); time.sleep(2); open(sys.argv[1], "w")' "$REVIEW_FIXTURE/late-launch" "$REVIEW_FIXTURE/client-waiting" >/dev/null 2>&1 &
    fi
    if test -f "$REVIEW_FIXTURE/slow-start"; then sleep "$(cat "$REVIEW_FIXTURE/slow-start")"; fi
    # A supervised listener on the configured port, ended by `daemons stop` like a real daemon.
    if test -f "$REVIEW_FIXTURE/listen-port" && ! kill -0 "$(cat "$REVIEW_FIXTURE/pf-tracked-pid" 2>/dev/null)" 2>/dev/null; then
      python3 -c 'import socket,sys,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(); time.sleep(600)' "$(cat "$REVIEW_FIXTURE/listen-port")" </dev/null >/dev/null 2>&1 &
      echo $! >"$REVIEW_FIXTURE/pf-tracked-pid"
      sleep 0.3
    fi
    if test -f "$REVIEW_FIXTURE/daemons-started.json"; then
      cp "$REVIEW_FIXTURE/daemons-started.json" "$REVIEW_FIXTURE/daemons.json"
      touch "$REVIEW_FIXTURE/started"
    fi
    if test -f "$REVIEW_FIXTURE/fail-start"; then cat "$REVIEW_FIXTURE/fail-start" >&2; echo 'start failed' >&2; exit 1; fi ;;
  'run --skip-deps')
    # `mise run --skip-deps --no-timings <task> -- <args>`: echo what the task would receive.
    shift 3; task=$1; shift 2; printf '%s|' "$task" "$@"
    if test "$task" = fail; then exit 3; fi ;;
  'daemons logs')
    if test -f "$REVIEW_FIXTURE/logs.txt"; then cat "$REVIEW_FIXTURE/logs.txt"; else echo "Error: Daemon $4 not found" >&2; exit 1; fi ;;
  'daemons stop')
    if test -f "$REVIEW_FIXTURE/fail-stop"; then echo 'cannot stop' >&2; exit 1; fi
    if test -f "$REVIEW_FIXTURE/pf-tracked-pid"; then kill "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")" 2>/dev/null; fi
    # A stopped supervised listener is reported stopped, like a real daemon.
    if test -f "$REVIEW_FIXTURE/listen-port"; then
      python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); [ (e.__setitem__("status", "stopped"), e.pop("pid", None)) for e in d ]; json.dump(d, open(sys.argv[1], "w"))' "$REVIEW_FIXTURE/daemons.json"
    fi ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(mise, fs::Permissions::from_mode(0o755)).unwrap();
        // The supervisor as stack reaches it without a project: answers from files the test
        // controls, and "stops" a daemon by killing the PID it was told it tracks.
        let pitchfork = dir.path().join("bin/pitchfork");
        fs::write(
            &pitchfork,
            r#"#!/bin/sh
echo "$PITCHFORK_STATE_DIR $*" >>"$REVIEW_FIXTURE/pitchfork.log"
case "$1" in
  status)
    if test -f "$REVIEW_FIXTURE/pf-status.json"; then cat "$REVIEW_FIXTURE/pf-status.json"; exit 0; fi
    echo "Error: Daemon $3 not found" >&2; exit 1 ;;
  stop)
    if test -f "$REVIEW_FIXTURE/pf-fail-stop"; then echo 'cannot stop' >&2; exit 1; fi
    kill "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")" ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(pitchfork, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self { dir };
        fixture.ok(&["compile"]);
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_at(&self.dir.path().join("app"), args)
    }

    fn command_at(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .args(["-C", dir.to_str().unwrap()])
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

#[test]
fn container_mcp_scenario_rejects_a_server_that_only_returns_an_error() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.dir.path().join("appA")).unwrap();
    let fake = fixture.dir.path().join("bin/stack");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"structuredContent\":{\"ok\":false,\"error\":{\"code\":\"exec_failed\"}}}}'\nexit 1\n").unwrap();
    fs::set_permissions(fake, fs::Permissions::from_mode(0o755)).unwrap();
    let e2e = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let out = Command::new("bash")
        .arg(e2e.join("4-mcp.sh"))
        .env("STACK_E2E_ASSERT", e2e.join("assert.sh"))
        .env("STACK_E2E_TMP", fixture.dir.path())
        .env("STACK_E2E_WORK", fixture.dir.path())
        .env("STACK_E2E_BIN", fixture.dir.path().join("bin"))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "broken MCP passed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

impl Fixture {
    fn set_env(&self, vars: &[(&str, String)]) {
        let mut env: serde_json::Map<String, Value> =
            serde_json::from_slice(&fs::read(self.dir.path().join("env.json")).unwrap()).unwrap();
        env.retain(|k, _| k == "PATH");
        for (k, v) in vars {
            env.insert(k.to_string(), json!(v));
        }
        fs::write(
            self.dir.path().join("env.json"),
            Value::Object(env).to_string(),
        )
        .unwrap();
    }

    /// Run one `stack mcp` session and return each response's tool envelope by request id.
    fn mcp(&self, calls: &[(&str, Value)], caller: &[(&str, &str)]) -> Vec<Value> {
        let mut command = self.command(&["mcp"]);
        command.envs(caller.iter().copied());
        self.mcp_with(command, calls)
    }

    /// `mcp` with a prepared server command; the server must exit cleanly at end of input.
    fn mcp_with(&self, mut command: Command, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut input =
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string();
        input.push('\n');
        let app = self.dir.path().join("app");
        for (i, (name, args)) in calls.iter().enumerate() {
            let mut args = args.clone();
            args["dir"] = json!(app);
            let call = json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":name,"arguments":args}});
            input.push_str(&format!("{call}\n"));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "MCP server failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let responses: Vec<Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (1..=calls.len())
            .map(|id| {
                let r = responses.iter().find(|r| r["id"] == id).unwrap();
                r["result"].clone()
            })
            .collect()
    }
}

const PRINT_ENDPOINTS: &str = r#"for v in DATABASE_URL PGHOST PGPORT PGUSER REDIS_URL WEB_URL STACK_UNVERIFIED; do eval "printf '%s=%s\n' $v \"\${$v-ABSENT}\""; done"#;

fn printed(stdout: &str) -> std::collections::BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn unverified_endpoints_keep_an_unresolvable_host_for_every_host_form() {
    let fixture = Fixture::with_bundle(
        "[bundle]\nname='test'\n[services.postgres]\npreset='postgres'\n[services.redis]\npreset='redis'\n",
    );
    // The caller's own values must not leak through either, whether poisoned or removed.
    let caller = [
        ("DATABASE_URL", "postgresql://caller@localhost:5432/caller"),
        ("PGHOST", "localhost"),
        ("PGPORT", "5432"),
        ("PGUSER", "caller"),
        ("REDIS_URL", "redis://localhost:6379"),
    ];
    for (url_host, bare_host) in [
        ("127.0.0.1", "127.0.0.1"),
        ("[::1]", "::1"),
        ("db.internal", "db.internal"),
    ] {
        fixture.set_env(&[
            (
                "DATABASE_URL",
                format!("postgresql://app:pw@{url_host}:5432/db?sslmode=disable"),
            ),
            ("PGHOST", bare_host.into()),
            ("PGPORT", "5432".into()),
            ("PGUSER", "app".into()),
            ("REDIS_URL", format!("redis://:pw@{url_host}:6379/0")),
        ]);
        let expected: std::collections::BTreeMap<String, String> = [
            (
                "DATABASE_URL",
                "postgresql://app:pw@unverified.stack.invalid:5432/db?sslmode=disable",
            ),
            ("PGHOST", "unverified.stack.invalid"),
            ("PGPORT", "ABSENT"),
            ("PGUSER", "ABSENT"),
            ("REDIS_URL", "redis://:pw@unverified.stack.invalid:6379/0"),
            ("WEB_URL", "ABSENT"),
            ("STACK_UNVERIFIED", "postgres,redis"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let out = fixture
            .command(&["exec", "--", "sh", "-c", PRINT_ENDPOINTS])
            .envs(caller)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            printed(&String::from_utf8_lossy(&out.stdout)),
            expected,
            "CLI, {url_host}"
        );

        let results = fixture.mcp(
            &[
                (
                    "stack_exec",
                    json!({ "command": ["sh", "-c", PRINT_ENDPOINTS] }),
                ),
                (
                    "stack_exec",
                    json!({ "command": ["true"], "require": ["postgres"] }),
                ),
            ],
            &caller,
        );
        let data = &results[0]["structuredContent"]["data"];
        assert_eq!(data["exit_code"], 0, "{data}");
        assert_eq!(
            printed(data["stdout"].as_str().unwrap()),
            expected,
            "MCP, {url_host}"
        );
        assert_eq!(
            results[1]["structuredContent"]["error"]["code"],
            "service_unavailable"
        );

        let out = fixture
            .command(&["exec", "--json", "--require", "postgres", "--", "true"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
            "service_unavailable"
        );
    }
}

#[test]
fn verified_endpoints_pass_through_and_are_poisoned_once_verification_fails() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let inspect: Value =
        serde_json::from_slice(&fixture.ok(&["inspect", "--json"]).stdout).unwrap();
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap();
    fixture.set_env(&[("WEB_URL", format!("http://[::1]:{port}/health"))]);
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "name": "web", "status": "running", "pid": std::process::id(), "port": port }])
            .to_string(),
    )
    .unwrap();

    // Accept connections only once the provider has started, so `up` sees nothing to stop first.
    let up = fixture
        .command(&["up", "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fixture.dir.path().join("started").exists() {
        assert!(Instant::now() < deadline, "provider start never ran");
        thread::sleep(Duration::from_millis(10));
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", port as u16)).unwrap();
    let out = up.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let verified = format!("http://[::1]:{port}/health");
    let out = fixture.ok(&[
        "exec",
        "--require",
        "web",
        "--",
        "sh",
        "-c",
        PRINT_ENDPOINTS,
    ]);
    let seen = printed(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(seen["WEB_URL"], verified);
    assert_eq!(seen["STACK_UNVERIFIED"], "ABSENT");
    let results = fixture.mcp(
        &[(
            "stack_exec",
            json!({ "command": ["sh", "-c", PRINT_ENDPOINTS] }),
        )],
        &[],
    );
    let seen = printed(
        results[0]["structuredContent"]["data"]["stdout"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(seen["WEB_URL"], verified);

    drop(listener);
    let out = fixture.ok(&["exec", "--", "sh", "-c", PRINT_ENDPOINTS]);
    let seen = printed(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        seen["WEB_URL"],
        format!("http://unverified.stack.invalid:{port}/health")
    );
    assert_eq!(seen["STACK_UNVERIFIED"], "web");
}

/// Counts connections to a TCP port and a Unix socket that a libpq client would use for it,
/// closing each at once so the client fails fast.
/// Each named variable's raw bytes as the command saw them, or `None` when unset.
const DUMP_RAW: &str = r#"for v in UNRELATED_RAW DATABASE_URL PGHOST PGHOSTADDR PGPASSWORD; do eval "if test \"\${$v+set}\"; then printf '%s=%s\n' $v \"\$$v\"; fi"; done >"$REVIEW_FIXTURE/raw.out""#;

fn dumped(fixture: &Fixture) -> std::collections::BTreeMap<String, Vec<u8>> {
    let raw = fs::read(fixture.dir.path().join("raw.out")).unwrap();
    fs::remove_file(fixture.dir.path().join("raw.out")).unwrap();
    raw.split(|b| *b == b'\n')
        .filter_map(|line| {
            let eq = line.iter().position(|b| *b == b'=')?;
            Some((
                String::from_utf8(line[..eq].to_vec()).unwrap(),
                line[eq + 1..].to_vec(),
            ))
        })
        .collect()
}

#[test]
fn non_unicode_caller_variables_are_inherited_raw_or_withheld_without_crashing() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let unrelated: &[u8] = b"raw\xff\xfebytes";
    let raw_url: &[u8] = b"postgresql://u@127.0.0.1:5432/db\xff";
    let caller: [(&str, &[u8]); 5] = [
        ("UNRELATED_RAW", unrelated),
        ("DATABASE_URL", raw_url),
        ("PGHOST", b"localhost\xff"),
        ("PGHOSTADDR", b"127.0.0.1\xff"),
        ("PGPASSWORD", b"secret\xff"),
    ];
    let with_caller = |fixture: &Fixture, args: &[&str], vars: &[(&str, &[u8])]| {
        let mut command = fixture.command(args);
        for (k, v) in vars {
            command.env(k, OsStr::from_bytes(v));
        }
        command
    };
    let exec = ["exec", "--", "sh", "-c", DUMP_RAW];
    let mcp_exec = ("stack_exec", json!({ "command": ["sh", "-c", DUMP_RAW] }));

    // No services: commands inherit an unrelated value byte for byte.
    let fixture = Fixture::new();
    let out = with_caller(&fixture, &exec, &caller[..1]).output().unwrap();
    assert!(
        out.status.success(),
        "CLI exec: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(dumped(&fixture)["UNRELATED_RAW"], unrelated, "CLI exec");
    let results = fixture.mcp_with(
        with_caller(&fixture, &["mcp"], &caller[..1]),
        std::slice::from_ref(&mcp_exec),
    );
    assert_eq!(results[0]["structuredContent"]["data"]["exit_code"], 0);
    assert_eq!(dumped(&fixture)["UNRELATED_RAW"], unrelated, "MCP exec");

    // An unverified service: relevant values are poisoned or removed even when not Unicode,
    // and unrelated ones still pass through untouched.
    let fixture =
        Fixture::with_bundle("[bundle]\nname='test'\n[services.postgres]\npreset='postgres'\n");
    let check_withheld = |context: &str| {
        let seen = dumped(&fixture);
        assert_eq!(seen["UNRELATED_RAW"], unrelated, "{context}");
        let url = String::from_utf8(seen["DATABASE_URL"].clone())
            .unwrap_or_else(|_| panic!("{context}: raw DATABASE_URL passed through"));
        assert!(
            url.starts_with("postgresql://u@unverified.stack.invalid:5432/"),
            "{context}: {url}"
        );
        assert_eq!(seen["PGHOST"], UNVERIFIED.as_bytes(), "{context}");
        assert_eq!(seen["PGHOSTADDR"], UNVERIFIED.as_bytes(), "{context}");
        assert!(!seen.contains_key("PGPASSWORD"), "{context}");
    };
    let out = with_caller(&fixture, &exec, &caller).output().unwrap();
    assert!(
        out.status.success(),
        "CLI exec: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    check_withheld("CLI exec");

    let out = with_caller(&fixture, &["status", "--json"], &caller)
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("CLI status: {}", String::from_utf8_lossy(&out.stderr)));
    let withheld = &status["data"]["checks"][0]["withheld"];
    for var in ["DATABASE_URL", "PGHOST", "PGHOSTADDR", "PGPASSWORD"] {
        assert!(
            withheld.as_array().unwrap().contains(&json!(var)),
            "CLI status: {status}"
        );
    }

    let results = fixture.mcp_with(
        with_caller(&fixture, &["mcp"], &caller),
        &[("stack_status", json!({})), mcp_exec],
    );
    assert_eq!(
        results[0]["structuredContent"]["data"]["checks"][0]["withheld"], *withheld,
        "MCP status"
    );
    assert_eq!(results[1]["structuredContent"]["data"]["exit_code"], 0);
    check_withheld("MCP exec");
}

struct Listeners {
    port: u16,
    socket_dir: TempDir,
    seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Listeners {
    fn start() -> Self {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::Ordering;
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        let socket_dir = tempfile::Builder::new()
            .prefix("pg")
            .tempdir_in("/tmp")
            .unwrap();
        let unix = UnixListener::bind(socket_dir.path().join(format!(".s.PGSQL.{port}"))).unwrap();
        tcp.set_nonblocking(true).unwrap();
        unix.set_nonblocking(true).unwrap();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = seen.clone();
        thread::spawn(move || loop {
            if tcp.accept().is_ok() || unix.accept().is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(5));
        });
        Self {
            port,
            socket_dir,
            seen,
        }
    }

    fn seen(&self) -> usize {
        // Let a connection that was just made reach the accept loop.
        thread::sleep(Duration::from_millis(50));
        self.seen.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Connect once with the URL and once from libpq's environment variables alone.
const PSQL: &str = r#"psql -w -Atc 'select 1' "${DATABASE_URL-}" 2>&1; echo "status=$?"; psql -w -Atc 'select 1' 2>&1; echo "status=$?""#;

fn assert_refused(output: &str, context: &str) {
    assert_eq!(
        output.matches("status=2").count(),
        2,
        "{context}: psql did not fail to connect: {output}"
    );
    assert!(
        output.matches(UNVERIFIED).count() >= 2,
        "{context}: psql did not try the invalid host: {output}"
    );
}

const UNVERIFIED: &str = "unverified.stack.invalid";

#[test]
fn libpq_never_reaches_a_listener_through_a_withheld_postgres_endpoint() {
    let Some(psql) = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("psql"))
        .find(|p| p.is_file())
    else {
        eprintln!("skipping: psql is not on PATH");
        return;
    };
    let fixture =
        Fixture::with_bundle("[bundle]\nname='test'\n[services.postgres]\npreset='postgres'\n");
    let listeners = Listeners::start();
    let (port, dir) = (
        listeners.port.to_string(),
        listeners.socket_dir.path().display().to_string(),
    );
    let at = |url: &str| url.replace("PORT", &port).replace("DIR", &dir);
    type Vars = Vec<(&'static str, String)>;
    // Each case reaches a listener without stack: (provider variables, caller variables).
    let cases: Vec<(&str, Vars, Vars)> = vec![
        (
            "host parameter after #",
            vec![(
                "DATABASE_URL",
                at("postgresql://u@localhost:PORT/db#x?host=127.0.0.1"),
            )],
            vec![],
        ),
        (
            "hostaddr parameter after #",
            vec![(
                "DATABASE_URL",
                at("postgresql://u@localhost:PORT/db#x?hostaddr=127.0.0.1"),
            )],
            vec![],
        ),
        (
            "socket host parameter",
            vec![(
                "DATABASE_URL",
                at("postgresql://u@localhost:PORT/db?host=DIR"),
            )],
            vec![],
        ),
        (
            "provider PGHOSTADDR",
            vec![
                ("DATABASE_URL", at("postgresql://u@db.invalid:PORT/db")),
                ("PGHOSTADDR", "127.0.0.1".into()),
            ],
            vec![],
        ),
        (
            "caller PGHOSTADDR",
            vec![("DATABASE_URL", at("postgresql://u@db.invalid:PORT/db"))],
            vec![("PGHOSTADDR", "127.0.0.1".into())],
        ),
        (
            "keywords with spaces around =",
            vec![("DATABASE_URL", at("host = DIR port = PORT dbname = app"))],
            vec![],
        ),
        (
            "keywords with a quoted value",
            vec![(
                "DATABASE_URL",
                at("host=DIR password='has space' port=PORT dbname=app"),
            )],
            vec![],
        ),
        (
            "empty URL with caller defaults",
            vec![("DATABASE_URL", String::new())],
            vec![("PGHOST", dir.clone()), ("PGPORT", port.clone())],
        ),
        (
            "caller-only endpoints",
            vec![],
            vec![
                ("DATABASE_URL", at("postgresql://u@127.0.0.1:PORT/db")),
                ("PGHOST", dir.clone()),
                ("PGPORT", port.clone()),
            ],
        ),
    ];
    for (name, provider, caller) in cases {
        let mut caller = caller;
        caller.push(("PGCONNECT_TIMEOUT", "3".into()));
        // Control: the unpoisoned configuration does reach a listener.
        let before = listeners.seen();
        Command::new(&psql)
            .args(["-w", "-Atc", "select 1"])
            .args(
                // The provider's value wins over the caller's, as in `stack exec`.
                caller
                    .iter()
                    .chain(&provider)
                    .rfind(|(k, _)| *k == "DATABASE_URL")
                    .map(|(_, v)| v),
            )
            .envs(caller.iter().cloned())
            .envs(provider.iter().cloned())
            .output()
            .unwrap();
        assert!(listeners.seen() > before, "{name}: control never connected");

        fixture.set_env(&provider);
        let before = listeners.seen();
        let out = fixture
            .command(&["exec", "--", "sh", "-c", PSQL])
            .envs(caller.iter().cloned())
            .output()
            .unwrap();
        assert_refused(
            &String::from_utf8_lossy(&out.stdout),
            &format!("CLI, {name}"),
        );
        let caller_refs: Vec<(&str, &str)> = caller.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let results = fixture.mcp(
            &[("stack_exec", json!({ "command": ["sh", "-c", PSQL] }))],
            &caller_refs,
        );
        let data = &results[0]["structuredContent"]["data"];
        assert_refused(data["stdout"].as_str().unwrap(), &format!("MCP, {name}"));
        assert_eq!(
            listeners.seen(),
            before,
            "{name}: a withheld endpoint was reached"
        );
    }
}

#[test]
fn mcp_rejects_owner_pids_outside_the_supported_range_before_lifecycle_work() {
    let fixture = Fixture::new();
    let log = fixture.dir.path().join("mise.log");
    let _ = fs::remove_file(&log);
    let invalid = [
        json!(4_294_967_296u64),
        json!(u64::MAX),
        json!(4_294_967_297u64),
        json!(2_147_483_648u64),
        json!(0),
        json!(-1),
        json!(1.5),
        json!("123"),
        json!(true),
    ];
    let calls: Vec<(&str, Value)> = invalid
        .iter()
        .map(|pid| ("stack_up", json!({ "owner_pid": pid })))
        .collect();
    for (result, pid) in fixture.mcp(&calls, &[]).iter().zip(&invalid) {
        assert_eq!(result["isError"], true, "{pid}");
        assert_eq!(
            result["structuredContent"]["error"]["code"], "usage",
            "{pid}"
        );
    }
    assert!(!log.exists(), "provider work ran for an invalid owner_pid");
    assert!(!fixture.dir.path().join("app/.stack/session.json").exists());
    assert!(!fixture.dir.path().join("state/sessions").exists());

    let pid = std::process::id();
    let results = fixture.mcp(
        &[
            ("stack_up", json!({ "owner_pid": pid })),
            ("stack_up", json!({ "owner_pid": null })),
            ("stack_up", json!({})),
        ],
        &[],
    );
    assert_eq!(
        results[0]["structuredContent"]["data"]["session"]["lease"]["owner_pid"],
        pid
    );
    for result in &results[1..] {
        assert_eq!(result["isError"], false);
        assert!(result["structuredContent"]["data"]["session"]
            .get("lease")
            .is_none());
    }
    assert!(log.exists(), "a valid stack_up must reach the provider");

    for bad in ["0", "2147483648", "4294967296"] {
        let out = fixture
            .command(&["up", "--owner-pid", bad])
            .output()
            .unwrap();
        assert!(!out.status.success(), "--owner-pid {bad} was accepted");
    }
}

#[test]
fn tools_only_projects_never_need_the_service_supervisor() {
    let fixture = Fixture::new();
    // `mise daemons` is experimental and unconfigured without services; any query would fail.
    fs::write(fixture.dir.path().join("fail-query"), "").unwrap();
    fixture.ok(&["up"]);
    fixture.ok(&["status"]);
    fixture.ok(&["exec", "--", "true"]);
    fixture.ok(&["down"]);
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("daemons"), "supervisor queried:\n{log}");
}

#[test]
fn repeated_up_of_an_unchanged_generation_keeps_the_session_id() {
    let fixture = Fixture::new();
    let id = |out: Output| serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"]["session"]["id"].clone();
    let first = id(fixture.ok(&["up", "--json"]));
    assert_eq!(id(fixture.ok(&["up", "--json"])), first);
    fs::write(
        fixture.dir.path().join("app/stack.toml"),
        "[[use]]\nbundle='path:../bundle'\n[env]\nMODE='changed'\n",
    )
    .unwrap();
    assert_ne!(id(fixture.ok(&["up", "--json"])), first);
}

#[test]
fn gc_fails_when_an_expired_session_cannot_be_stopped() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "1s"]);
    let paths = fixture.session_paths();
    let mut session: Value = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
    session["services"] = json!({ "worker": { "port":0, "pid":std::process::id(), "identity":"liveness", "verified_at":0 } });
    for path in &paths {
        fs::write(path, session.to_string()).unwrap();
    }
    fs::write(fixture.dir.path().join("fail-stop"), "").unwrap();
    thread::sleep(Duration::from_secs(2));
    let out = fixture.command(&["gc", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "gc_incomplete");
    assert_eq!(result["error"]["details"][0]["stopped"], false);
    assert!(paths.iter().all(|p| p.exists()), "ownership kept for a retry");
}

#[test]
fn exec_json_reports_the_command_in_one_object_and_keeps_its_exit_code() {
    let fixture = Fixture::new();
    let out = fixture
        .command(&["--json", "exec", "--", "sh", "-c", "echo out; echo err >&2; exit 3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let result: Value = serde_json::from_slice(&out.stdout).expect("stdout is exactly one JSON object");
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["exit_code"], 3);
    assert_eq!(result["data"]["stdout"], "out\n");
    assert_eq!(result["data"]["stderr"], "err\n");

    // A command stack had to kill did not do its job: an error, with the output so far.
    let out = fixture
        .command(&["--json", "exec", "--timeout", "1s", "--", "sh", "-c", "echo partial; sleep 10"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["code"], "timed_out");
    assert_eq!(result["error"]["details"][0]["timed_out"], true);
    assert_eq!(result["error"]["details"][0]["stdout"], "partial\n");
}

#[test]
fn exec_timeout_without_json_keeps_the_output_and_kills_the_whole_command() {
    let fixture = Fixture::new();
    let start = Instant::now();
    let out = fixture
        .command(&["exec", "--timeout", "1s", "--", "sh", "-c", "sleep 30 & echo $!; wait"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124));
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert!(String::from_utf8_lossy(&out.stderr).contains("did not finish within 1s"));
    let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_alive(pid), "descendant {pid} survived the deadline");

    let out = fixture.command(&["exec", "--timeout", "10s", "--", "sh", "-c", "echo done; exit 7"]).output().unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}

#[test]
fn a_session_copied_in_from_another_checkout_is_ignored_and_never_committed() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let ignore = fixture.dir.path().join("app/.stack/.gitignore");
    assert_eq!(fs::read_to_string(&ignore).unwrap(), "*\n");

    // A worktree or clone that received the project copy (say, committed by mistake).
    let other = fixture.dir.path().join("other");
    fs::create_dir_all(other.join(".stack")).unwrap();
    fs::write(other.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
    fs::copy(fixture.dir.path().join("app/.stack/session.json"), other.join(".stack/session.json")).unwrap();
    fs::copy(fixture.dir.path().join("app/stack.lock"), other.join("stack.lock")).unwrap();
    let out = fixture.command_at(&other, &["status", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["session"], Value::Null, "{result}");
    fixture.ok(&["down"]);
}

#[test]
fn restart_replaces_only_the_named_service_and_keeps_the_session() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nwatch=['main.py']\n");
    fs::write(fixture.dir.path().join("app/main.py"), "v1").unwrap();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let before = up["data"]["checks"][0]["pid"].as_u64().unwrap();
    assert!(up["data"]["session"]["services"]["web"]["started_at_ms"].is_u64(), "{up}");

    // Edited after the service started: reported, not a verification failure.
    thread::sleep(Duration::from_millis(1100));
    fs::write(fixture.dir.path().join("app/main.py"), "v2").unwrap();
    let status = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!(status["data"]["checks"][0]["ready"], true);
    assert_eq!(status["data"]["checks"][0]["changed_since_start"], json!(["main.py"]), "{status}");
    let out = fixture.ok(&["exec", "--", "true"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("stack restart web"));

    let restarted = json_result(&fixture.ok(&["restart", "web", "--json"]));
    assert_eq!(restarted["data"]["restarted"], json!(["web"]));
    assert_eq!(restarted["data"]["session"]["id"], up["data"]["session"]["id"]);
    let after = restarted["data"]["checks"][0]["pid"].as_u64().unwrap();
    assert_ne!(after, before, "a new process");
    assert!(!pid_alive(before as u32));
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(log.contains("daemons stop -- web") && log.contains("daemons start -- web"), "{log}");

    // The new process is the verified one, and it loaded the edit.
    let status = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!(status["data"]["healthy"], true, "{status}");
    assert!(status["data"]["checks"][0].get("changed_since_start").is_none(), "{status}");
    fixture.ok(&["exec", "--require", "web", "--", "true"]);

    // Logs since the start ask the supervisor for that instant onwards.
    fs::write(fixture.dir.path().join("logs.txt"), "new\n").unwrap();
    let logs = json_result(&fixture.ok(&["logs", "web", "--since-start", "--json"]));
    assert_eq!(logs["data"]["since_start"], true);
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(log.lines().any(|l| l.starts_with("daemons logs -- web") && l.contains("--since 20")), "{log}");

    let out = fixture.command(&["restart", "nope", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "unknown_service");
    fixture.ok(&["down"]);
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "no_session");
}

#[test]
fn a_failed_restart_records_the_replacement_it_launched() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let old = up["data"]["session"]["services"]["web"]["pid"].as_u64().unwrap();
    fs::write(fixture.dir.path().join("fail-env-after-start"), "").unwrap();
    let out = fixture.command(&["restart", "web", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let replacement: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    assert_ne!(u64::from(replacement), old);
    for path in fixture.session_paths() {
        let record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["launching"], true, "{record}");
        assert_eq!(record["services"]["web"]["pid"], replacement, "{record}");
    }
    fs::remove_file(fixture.dir.path().join("fail-env-after-start")).unwrap();
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["healthy"], false, "{status}");
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "session_stale");

    // A deleted checkout's GC keeps the incomplete record rather than releasing a live service.
    let moved = fixture.dir.path().join("moved");
    fs::rename(fixture.dir.path().join("app"), &moved).unwrap();
    let gc = json_result(&fixture.command_at(fixture.dir.path(), &["gc", "--json"]).output().unwrap());
    assert_eq!(gc["ok"], false, "{gc}");
    assert_eq!(fixture.index_files(), 1);
    assert!(pid_alive(replacement));
    fs::rename(&moved, fixture.dir.path().join("app")).unwrap();
    fixture.ok(&["up"]);
    fixture.ok(&["exec", "--require", "web", "--", "true"]);
    fixture.ok(&["down"]);
    assert!(!pid_alive(replacement));
}

#[test]
fn retrying_an_incomplete_launch_keeps_what_it_recorded_until_down_confirms_it() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    fs::write(fixture.dir.path().join("fail-env-after-start"), "").unwrap();
    assert!(!fixture.command(&["restart", "web", "--json"]).output().unwrap().status.success());
    let replacement: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    fs::remove_file(fixture.dir.path().join("fail-env-after-start")).unwrap();

    // The recovery `up` fails right after writing its launch record.
    fs::write(fixture.dir.path().join("fail-query-after-one"), "").unwrap();
    let retry = json_result(&fixture.command(&["up", "--json"]).output().unwrap());
    assert_eq!(retry["ok"], false, "{retry}");
    fs::remove_file(fixture.dir.path().join("fail-query-after-one")).unwrap();
    let record: Value = serde_json::from_slice(&fs::read(&fixture.session_paths()[0]).unwrap()).unwrap();
    assert_eq!(record["services"]["web"]["pid"], replacement, "{record}");

    // Even with the supervisor listing nothing, the recorded process is stopped, not abandoned.
    fs::write(fixture.dir.path().join("daemons.json"), "[]").unwrap();
    let down = json_result(&fixture.command(&["down", "--json"]).output().unwrap());
    let alive = pid_alive(replacement);
    if alive {
        Command::new("kill").arg(replacement.to_string()).status().unwrap();
    }
    assert!(down["ok"] == false || !alive, "down confirmed while {replacement} lived: {down}");
}

#[test]
fn a_legacy_session_copy_is_ignored_unless_it_may_be_the_last_record_of_a_live_service() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let mut legacy: Value = serde_json::from_slice(&fs::read(fixture.dir.path().join("app/.stack/session.json")).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("project_dir_id");
    legacy.as_object_mut().unwrap().remove("project_dir_created");
    let other = fixture.dir.path().join("other");
    fs::create_dir_all(other.join(".stack")).unwrap();
    fs::copy(fixture.dir.path().join("app/stack.toml"), other.join("stack.toml")).unwrap();
    fs::copy(fixture.dir.path().join("app/stack.lock"), other.join("stack.lock")).unwrap();
    fs::write(other.join(".stack/session.json"), legacy.to_string()).unwrap();
    let status = || json_result(&fixture.command_at(&other, &["status", "--json"]).output().unwrap());

    // The original checkout still holds the record in the machine index.
    assert_eq!(status()["data"]["session"], Value::Null);
    // Without that index entry, the copy may be the last record of the running service.
    let index = fixture.session_paths().into_iter().nth(1).unwrap();
    let saved = fs::read(&index).unwrap();
    fs::remove_file(&index).unwrap();
    assert_eq!(status()["error"]["code"], "session_conflict");
    // Once the supervisor confirms nothing it names runs, the copy describes nothing and is ignored.
    fs::write(&index, saved).unwrap();
    fixture.ok(&["down"]);
    assert_eq!(status()["data"]["session"], Value::Null);
    // A launch that was in progress may still register services, so that copy is kept.
    legacy["launching"] = Value::Bool(true);
    fs::write(other.join(".stack/session.json"), legacy.to_string()).unwrap();
    assert_eq!(status()["error"]["code"], "session_conflict");
}

#[test]
fn a_moved_checkouts_only_record_is_kept_and_named() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    for path in fixture.session_paths().into_iter().skip(1) {
        fs::remove_file(path).unwrap();
    }
    let moved = fixture.dir.path().join("moved");
    fs::rename(fixture.dir.path().join("app"), &moved).unwrap();
    fs::write(fixture.dir.path().join("daemons.json"), "[]").unwrap();
    for command in [&["status", "--json"][..], &["down", "--json"]] {
        let result = json_result(&fixture.command_at(&moved, command).output().unwrap());
        assert_eq!(result["error"]["code"], "session_conflict", "{command:?}: {result}");
        assert!(result["error"]["hint"].as_str().unwrap().contains("move the directory back"), "{result}");
    }
    assert!(moved.join(".stack/session.json").exists(), "the last record is kept");
    assert!(pid_alive(pid));
    Command::new("kill").arg(pid.to_string()).status().unwrap();
}

#[test]
fn mcp_restart_rejects_a_malformed_service_list_before_stopping_anything() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let _ = fs::remove_file(fixture.dir.path().join("mise.log"));
    let results = fixture.mcp(
        &[("stack_restart", json!({ "services": "web" })), ("stack_restart", json!({ "services": [123] }))],
        &[],
    );
    for result in &results {
        assert_eq!(result["structuredContent"]["error"]["code"], "usage", "{result}");
    }
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap_or_default();
    assert!(!log.contains("daemons stop"), "{log}");
    fixture.ok(&["down"]);
}

#[test]
fn watch_compares_modification_times_below_a_second() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nwatch=['main.py']\n");
    let file = fixture.dir.path().join("app/main.py");
    fs::write(&file, "before").unwrap();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let start = up["data"]["session"]["services"]["web"]["started_at_ms"].as_u64().unwrap();
    let set_mtime = |ms: u64| {
        let file = fs::File::options().write(true).open(&file).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(ms)).unwrap();
    };
    let changed = || json_result(&fixture.ok(&["status", "--json"]))["data"]["checks"][0]["changed_since_start"].clone();
    set_mtime(start - 1);
    assert_eq!(changed(), Value::Null);
    set_mtime(start + 1);
    assert_eq!(changed(), json!(["main.py"]));
    fixture.ok(&["down"]);
}

#[test]
fn a_watch_too_large_to_scan_is_reported_incomplete() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nwatch=['wide']\n");
    let wide = fixture.dir.path().join("app/wide");
    fs::create_dir(&wide).unwrap();
    for i in 0..20_010 {
        fs::File::create(wide.join(i.to_string())).unwrap();
    }
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let start = Instant::now();
    let out = fixture.ok(&["status", "--json"]);
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(json_result(&out)["data"]["checks"][0]["watch_incomplete"], true);
    fixture.ok(&["down"]);
}

#[test]
fn up_keeps_a_legacy_process_start_time_unknown() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let pid = up["data"]["session"]["services"]["web"]["pid"].clone();
    for path in fixture.session_paths() {
        let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["services"]["web"].as_object_mut().unwrap().remove("started_at_ms");
        fs::write(path, record.to_string()).unwrap();
    }
    let again = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(again["data"]["session"]["services"]["web"]["pid"], pid);
    assert_eq!(again["data"]["session"]["services"]["web"].get("started_at_ms"), None, "{again}");
    let out = fixture.command(&["logs", "web", "--since-start", "--json"]).output().unwrap();
    assert!(json_result(&out)["error"]["hint"].as_str().unwrap().contains("stack restart"));
    fixture.ok(&["down"]);
}

#[test]
fn exec_with_a_timeout_passes_termination_on_to_the_command() {
    let fixture = Fixture::new();
    let marker = fixture.dir.path().join("child-pid");
    let mut stack = fixture
        .command(&["exec", "--timeout", "60s", "--", "sh", "-c", &format!("echo $$ > {}; exec sleep 60", marker.display())])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while fs::read_to_string(&marker).map_or(true, |s| s.trim().is_empty()) {
        assert!(Instant::now() < deadline, "command never started");
        thread::sleep(Duration::from_millis(20));
    }
    let child: u32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(stack.id() as i32, libc::SIGTERM) };
    let status = stack.wait().unwrap();
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(!pid_alive(child), "command {child} outlived stack");
}

#[test]
fn restart_refuses_a_session_started_from_another_configuration() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    fs::write(
        fixture.dir.path().join("app/stack.toml"),
        "[[use]]\nbundle='path:../bundle'\n[env]\nMODE='changed'\n",
    )
    .unwrap();
    let inspect = json_result(&fixture.ok(&["inspect", "--json"]));
    assert!(inspect["data"]["warnings"].to_string().contains("different configuration"), "{inspect}");
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "session_stale");
    fixture.ok(&["down"]);
}

#[test]
fn inspect_lists_custom_services() {
    let fixture = Fixture::with_bundle(WEB);
    let out = fixture.ok(&["inspect"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("service web (custom, bundle:test)"));
}

#[test]
fn argument_errors_honour_json() {
    let fixture = Fixture::new();
    for args in [
        &["--json", "up", "--owner-pid", "0"][..],
        &["--json", "no-such-command"],
        &["exec", "--json"],
    ] {
        let out = fixture.command(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let result: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        assert_eq!(result["error"]["code"], "usage", "{args:?}");
    }
    // After `--`, `--json` belongs to the command, so stack's own error stays text.
    let out = fixture.command(&["exec", "--bogus", "--", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn a_failed_start_keeps_ownership_even_after_the_service_is_removed() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "name": "web", "status": "running", "pid": std::process::id() }]).to_string(),
    )
    .unwrap();
    fs::write(fixture.dir.path().join("fail-start"), "").unwrap();
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "start_failed");

    // The launch record withholds the service: nothing verified it.
    let status: Value = serde_json::from_slice(&fixture.command(&["status", "--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(status["data"]["checks"][0]["ready"], false);

    // Dropping the service also drops its port reservation; the launch record still names it.
    fs::write(fixture.dir.path().join("bundle/bundle.toml"), "[bundle]\nname='test'\n").unwrap();
    fixture.ok(&["compile"]);
    fs::write(fixture.dir.path().join("fail-query"), "").unwrap();
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert!(!out.status.success(), "down confirmed cleanup without asking the supervisor");
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"], "provider_failed");
    assert!(fixture.session_paths().iter().all(|p| p.exists()));
}

#[test]
fn a_failed_start_reports_the_services_error_without_the_supervisors_animation() {
    let fixture = Fixture::with_bundle(WEB);
    let frame = |n: u32| format!("{} [{WEB_ID}] waiting for delay (3s)...\n\r", char::from_u32(0x280b + n).unwrap());
    let said = |line: &str| format!("\u{2022} [{WEB_ID}] {line}\n");
    let output: String = [
        frame(0),
        said("Traceback (most recent call last):"),
        frame(1),
        said("  File \"/p/server.py\", line 2, in <module>"),
        frame(2),
        said("RuntimeError: bad edit"),
        frame(3),
        format!("\u{1b}[31mpitchfork ERROR\u{1b}[0m Daemon {WEB_ID} failed to start\n"),
    ]
    .concat();
    fs::write(fixture.dir.path().join("fail-start"), output).unwrap();
    let out = fixture.command(&["up", "--json"]).env("NO_COLOR", "1").env("CI", "1").output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "start_failed", "{result}");
    assert_eq!(
        result["error"]["details"][0]["output"],
        format!(
            "\u{2022} [{WEB_ID}] Traceback (most recent call last):\n\u{2022} [{WEB_ID}]   File \"/p/server.py\", line 2, in <module>\n\u{2022} [{WEB_ID}] RuntimeError: bad edit\npitchfork ERROR Daemon {WEB_ID} failed to start\nstart failed"
        ),
        "{result}"
    );
}

#[cfg(unix)]
#[test]
fn non_unicode_arguments_never_crash_usage_errors() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let out = fixture
        .command(&[])
        .arg(std::ffi::OsStr::from_bytes(b"--invalid-\xff"))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"], "usage");
}

#[test]
fn startup_time_does_not_consume_the_idle_ttl() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let inspect: Value = serde_json::from_slice(&fixture.ok(&["inspect", "--json"]).stdout).unwrap();
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "name": "web", "status": "running", "pid": std::process::id(), "port": port }]).to_string(),
    )
    .unwrap();
    fs::write(fixture.dir.path().join("slow-start"), "3").unwrap();
    let up = fixture.command(&["up", "--ttl", "2s", "--json"]).stdout(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fixture.dir.path().join("started").exists() {
        assert!(Instant::now() < deadline, "provider start never ran");
        thread::sleep(Duration::from_millis(10));
    }
    let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let out = up.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));

    // Startup took longer than the TTL; the lease must start from verification, not launch.
    let status: Value = serde_json::from_slice(&fixture.command(&["status", "--json"]).output().unwrap().stdout).unwrap();
    assert!(status["data"].get("lease_expired").is_none(), "{status}");
    let gc: Value = serde_json::from_slice(&fixture.ok(&["gc", "--json"]).stdout).unwrap();
    assert_eq!(gc["data"], json!([]));
}

#[test]
fn mise_resolution_with_no_matching_release_fails_before_writing_anything() {
    let fixture = Fixture::new();
    let app = fixture.dir.path().join("app");
    fs::write(app.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n[tools]\njq='1.7'\n").unwrap();
    let out = fixture.ok(&["compile", "--json"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["data"]["versions"][0]["resolved"], "1.7");
    let lock = fs::read(app.join("stack.lock")).unwrap();
    // `mise latest` exits 0 with empty output when nothing matches the prefix.
    fs::write(fixture.dir.path().join("latest-empty"), "").unwrap();
    fs::write(app.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n[tools]\njq='9.9'\n").unwrap();
    let out = fixture.command(&["compile", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let err: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(err["error"]["code"], "resolve_failed", "{err}");
    assert!(err["error"]["details"][0]["error"].as_str().unwrap().contains("no release matches"), "{err}");
    assert_eq!(fs::read(app.join("stack.lock")).unwrap(), lock);
    // Status and exec never resolve: with a stale request they refuse instead.
    let log = fixture.dir.path().join("mise.log");
    fs::remove_file(&log).unwrap();
    let out = fixture.command(&["exec", "--json", "--", "true"]).output().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"], "lock_outdated");
    assert!(!fs::read_to_string(&log).unwrap_or_default().contains("latest"));
}

#[test]
fn a_long_supervisor_socket_path_fails_before_install_and_doctor_reports_it() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let limit = if cfg!(target_os = "macos") { 104 } else { 108 };
    let suffix = "/sock/main.sock".len();
    // One byte over, made of two-byte characters so a character count would pass it.
    let long = |extra: usize| format!("/tmp/{}", "é".repeat((limit - suffix - 5 + extra) / 2) + &"x".repeat((limit - suffix - 5 + extra) % 2));
    let log = fixture.dir.path().join("mise.log");
    for (via_config, value) in [(true, long(1)), (false, long(1))] {
        let _ = fs::remove_file(&log);
        let mut command = fixture.command(&["up", "--json"]);
        if via_config {
            fixture.set_env(&[("PITCHFORK_STATE_DIR", value.clone())]);
        } else {
            fixture.set_env(&[]);
            command.env("PITCHFORK_STATE_DIR", &value);
        }
        let out = command.output().unwrap();
        assert!(!out.status.success());
        let err: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(err["error"]["code"], "socket_path_too_long", "{err}");
        assert_eq!(err["error"]["details"][0]["bytes"], limit + 1, "{err}");
        let steps = &err["error"]["details"][1]["steps"];
        assert_eq!(steps.as_array().unwrap().last().unwrap()["step"], "preflight", "{err}");
        assert_eq!(err["error"]["details"][1]["changed"], false);
        let calls = fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("install") && !calls.contains("daemons start"), "{calls}");

        let out = fixture.command(&["doctor", "--json"]).env("PITCHFORK_STATE_DIR", &value).output().unwrap();
        let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
        let check = doctor["error"]["details"].as_array().unwrap().iter().find(|c| c["name"] == "pitchfork_socket").cloned();
        assert_eq!(check.unwrap()["ok"], false, "{doctor}");
    }
    // Exactly at the limit is accepted, as Pitchfork accepts it.
    fixture.set_env(&[]);
    let out = fixture.command(&["doctor", "--json"]).env("PITCHFORK_STATE_DIR", long(0)).output().unwrap();
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = doctor["data"].as_array().or(doctor["error"]["details"].as_array()).unwrap().clone();
    let check = checks.iter().find(|c| c["name"] == "pitchfork_socket").unwrap();
    assert_eq!(check["ok"], true, "{doctor}");
    assert!(check["detail"].as_str().unwrap().contains(&format!("({limit} of {limit} bytes")), "{doctor}");
}

/// A TCP "service" whose answer the test controls: each connection gets the current reply
/// after the current delay, then is closed.
struct Responder {
    reply: std::sync::Arc<std::sync::Mutex<(Vec<u8>, Duration)>>,
}

impl Responder {
    fn start(port: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let reply = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), Duration::ZERO)));
        let shared = reply.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (bytes, delay) = shared.lock().unwrap().clone();
                thread::spawn(move || {
                    use std::io::Write;
                    thread::sleep(delay);
                    let _ = stream.write_all(&bytes);
                });
            }
        });
        Self { reply }
    }

    fn set(&self, bytes: impl Into<Vec<u8>>, delay: Duration) {
        *self.reply.lock().unwrap() = (bytes.into(), delay);
    }
}

const PROBED_WEB: &str = r#"[bundle]
name='test'
[services.web]
run='true'
[services.web.identity]
command = 'python3 {{bundle_dir}}/probe.py & echo $! >> "$REVIEW_FIXTURE/probe-pids"; wait $!'
timeout = '2s'
"#;

const PROBE_PY: &str = r#"import os, socket, sys
s = socket.create_connection(("127.0.0.1", int(os.environ["WEB_PORT"])), timeout=30)
data = b""
while True:
    chunk = s.recv(65536)
    if not chunk:
        break
    data += chunk
sys.stdout.write(data.decode("utf-8", "replace"))
assert "STACK_IDENTITY_WEB" not in os.environ
"#;

fn web_status(fixture: &Fixture) -> Value {
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"]["checks"][0].clone()
}

#[test]
fn identity_probes_accept_only_this_checkouts_instance() {
    let fixture = Fixture::with_bundle(PROBED_WEB);
    fs::write(fixture.dir.path().join("bundle/probe.py"), PROBE_PY).unwrap();
    fixture.ok(&["compile"]);
    let inspect: Value = serde_json::from_slice(&fixture.ok(&["inspect", "--json"]).stdout).unwrap();
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
    let config: toml::Table = fs::read_to_string(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap().parse().unwrap();
    let token = config["env"]["STACK_IDENTITY_WEB"].as_str().unwrap().to_string();
    assert!(token.starts_with("stack-") && token.len() > 30, "{token}");
    assert!(!config["daemons"]["web"].as_table().unwrap().contains_key("identity"), "probe is not provider config");
    fixture.set_env(&[("WEB_PORT", port.to_string()), ("STACK_IDENTITY_WEB", token.clone())]);
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "name": "web", "status": "running", "pid": std::process::id(), "port": port }]).to_string(),
    )
    .unwrap();

    // The correct instance verifies as an instance, not merely as alive.
    let up = fixture.command(&["up", "--json"]).stdout(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fixture.dir.path().join("started").exists() {
        assert!(Instant::now() < deadline, "provider start never ran");
        thread::sleep(Duration::from_millis(10));
    }
    let responder = Responder::start(port);
    responder.set(token.clone(), Duration::ZERO);
    let out = up.wait_with_output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["data"]["checks"][0]["identity"], "instance", "{result}");
    fixture.ok(&["exec", "--require", "web", "--", "true"]);

    let refused = |reply: Vec<u8>, delay: Duration, reason: &str| {
        responder.set(reply, delay);
        let start = Instant::now();
        let check = web_status(&fixture);
        assert!(start.elapsed() < Duration::from_secs(8), "probe was not bounded: {:?}", start.elapsed());
        assert_eq!(check["ready"], false, "{check}");
        assert!(check["reason"].as_str().unwrap().contains(reason), "{reason}: {check}");
        let out = fixture.command(&["exec", "--json", "--require", "web", "--", "true"]).output().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"], "service_unavailable");
        let out = fixture.ok(&["exec", "--", "sh", "-c", PRINT_ENDPOINTS]);
        assert_eq!(printed(&String::from_utf8_lossy(&out.stdout))["STACK_UNVERIFIED"], "web");
    };
    // A healthy server that is some other instance.
    refused(b"stack-0123456789abcdef0123456789abcdef".to_vec(), Duration::ZERO, "reached a different instance");
    // Missing and malformed output.
    refused(Vec::new(), Duration::ZERO, "printed no identity");
    refused(format!("{token}\nextra").into_bytes(), Duration::ZERO, "reached a different instance");
    refused(format!("prefix {token}").into_bytes(), Duration::ZERO, "reached a different instance");
    // Output flooding stays bounded and is rejected even when it ends with the token.
    let mut flood = vec![b'a'; 200_000];
    flood.extend_from_slice(token.as_bytes());
    refused(flood, Duration::ZERO, "more than 4096 bytes");
    // A probe that hangs is killed with its descendants at the deadline.
    let _ = fs::remove_file(fixture.dir.path().join("probe-pids"));
    refused(token.clone().into_bytes(), Duration::from_secs(20), "did not answer within 2s");
    let pids = fs::read_to_string(fixture.dir.path().join("probe-pids")).unwrap();
    thread::sleep(Duration::from_millis(200));
    for pid in pids.split_whitespace() {
        let alive = Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).status().unwrap().success();
        assert!(!alive, "probe descendant {pid} survived its deadline");
    }

    // Back to the right answer: verified again.
    responder.set(token.clone(), Duration::ZERO);
    assert_eq!(web_status(&fixture)["identity"], "instance");

    // A new token is a new generation: the running process cannot be this one any more.
    fs::remove_file(fixture.dir.path().join("state/identities.json")).unwrap();
    fixture.ok(&["compile"]);
    let config: toml::Table = fs::read_to_string(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap().parse().unwrap();
    assert_ne!(config["env"]["STACK_IDENTITY_WEB"].as_str().unwrap(), token);
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["data"]["stale"], true, "{status}");
    assert_eq!(status["data"]["checks"][0]["ready"], false);
}

#[test]
fn services_without_probes_stay_liveness_only() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n");
    let inspect: Value = serde_json::from_slice(&fixture.ok(&["inspect", "--json"]).stdout).unwrap();
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
    let config = fs::read_to_string(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap();
    assert!(!config.contains("STACK_IDENTITY"), "{config}");
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "name": "web", "status": "running", "pid": std::process::id(), "port": port }]).to_string(),
    )
    .unwrap();
    let up = fixture.command(&["up", "--json"]).stdout(Stdio::piped()).spawn().unwrap();
    while !fixture.dir.path().join("started").exists() {
        thread::sleep(Duration::from_millis(10));
    }
    let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let result: Value = serde_json::from_slice(&up.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(result["data"]["checks"][0]["identity"], "liveness", "{result}");
}

/// A detached process standing in for a supervised service. Its parent exits at once, so it is
/// reaped as soon as it is killed and its PID reads as gone; it is killed when dropped.
struct Detached(u32);

impl Detached {
    fn spawn() -> Self {
        let out = Command::new("sh").args(["-c", "sleep 300 >/dev/null 2>&1 & echo $!"]).output().unwrap();
        Self(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap())
    }

    fn alive(&self) -> bool {
        alive(self.0)
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.0.to_string()).stderr(Stdio::null()).status();
    }
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

const WEB: &str = "[bundle]\nname='test'\n[services.web]\nrun='true'\n";
const WEB_ID: &str = "app-0123456789abcdef/web";

struct Running {
    service: Detached,
    port: u16,
    listener: Option<std::net::TcpListener>,
}

impl Fixture {
    /// `stack up` of one supervised service that is really running and listening.
    fn start_web(&self, up_args: &[&str]) -> Running {
        let inspect: Value = serde_json::from_slice(&self.ok(&["inspect", "--json"]).stdout).unwrap();
        let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
        let service = Detached::spawn();
        fs::write(
            self.dir.path().join("daemons-started.json"),
            json!([{ "id": WEB_ID, "name": "web", "status": "running", "pid": service.0, "port": port }]).to_string(),
        )
        .unwrap();
        let mut args = vec!["up", "--json"];
        args.extend_from_slice(up_args);
        let up = self.command(&args).stdout(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.dir.path().join("started").exists() {
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
        fs::write(
            self.dir.path().join("pf-status.json"),
            json!({ "id": WEB_ID, "status": "running", "pid": pid, "active_port": port }).to_string(),
        )
        .unwrap();
        fs::write(self.dir.path().join("pf-tracked-pid"), pid.to_string()).unwrap();
    }

    /// Machine-wide gc, run from outside the project.
    fn gc(&self, extra: &[&str]) -> (bool, Value) {
        let mut args = vec!["gc", "--json"];
        args.extend_from_slice(extra);
        let out = self.command_at(self.dir.path(), &args).output().unwrap();
        (out.status.success(), serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
    }

    fn index_files(&self) -> usize {
        fs::read_dir(self.dir.path().join("state/sessions")).map_or(0, |d| d.count())
    }

    fn pitchfork_log(&self) -> String {
        fs::read_to_string(self.dir.path().join("pitchfork.log")).unwrap_or_default()
    }
}

#[test]
fn deleted_projects_are_retained_until_the_supervisor_confirms_terminal_cleanup() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    let index: Value = serde_json::from_slice(&fs::read(fs::read_dir(fixture.dir.path().join("state/sessions")).unwrap().next().unwrap().unwrap().path()).unwrap()).unwrap();
    assert_eq!(index["services"]["web"]["provider_id"], WEB_ID);
    let state_dir = index["provider"]["state_dir"].as_str().unwrap().to_string();
    assert!(index["provider"]["pitchfork"].as_str().unwrap().ends_with("bin/pitchfork"));
    assert!(index["project_dir_id"].is_array());

    fixture.supervisor_tracks(running.service.0, running.port);
    let app = fixture.dir.path().join("app");
    fs::remove_dir_all(&app).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    let entry = &result["error"]["details"][0];
    assert_eq!(entry["reason"], "project directory deleted");
    assert_eq!(entry["stopped"], false, "{result}");
    assert!(running.service.alive(), "GC must not issue a racy stop by daemon name");
    assert_eq!(fixture.index_files(), 1);
    assert!(!app.exists(), "cleanup recreated the deleted project");
    let log = fixture.pitchfork_log();
    assert!(log.contains(&format!("{state_dir} status --json {WEB_ID}")), "{log}");
    assert!(!log.contains(" stop "), "{log}");
    drop(running);
    thread::sleep(Duration::from_millis(100));
    fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID, "status": "stopped" }).to_string()).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    assert_eq!(fixture.index_files(), 0);
    assert_eq!(fixture.gc(&[]).1["data"], json!([]));
}

/// The recorded supervisor, moved where every path segment needs shell quoting.
fn awkward_provider(fixture: &Fixture) -> (String, String) {
    let weird = fixture.dir.path().join("we ird 'q' \"dq\" $HOME `id` $(id) ;|&*?\\ ü");
    fs::create_dir(&weird).unwrap();
    let pitchfork = weird.join("pitch fork");
    fs::copy(fixture.dir.path().join("bin/pitchfork"), &pitchfork).unwrap();
    let state_dir = weird.join("state $(touch pwned)");
    let index = fs::read_dir(fixture.dir.path().join("state/sessions")).unwrap().next().unwrap().unwrap().path();
    let mut session: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    session["provider"] = json!({ "pitchfork": pitchfork, "state_dir": state_dir });
    fs::write(&index, session.to_string()).unwrap();
    (pitchfork.to_str().unwrap().into(), state_dir.to_str().unwrap().into())
}

#[test]
fn a_deleted_checkouts_recovery_commands_reach_the_recorded_supervisor_verbatim() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    let (pitchfork, state_dir) = awkward_provider(&fixture);
    fixture.supervisor_tracks(running.service.0, running.port);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();

    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    let service = &result["error"]["details"][0]["services"][0];
    let recovery = &service["recovery"];
    assert_eq!(recovery["recorded_pid"], running.service.0, "{result}");
    assert_eq!(recovery["recorded_port"], running.port, "{result}");
    let (inspect, stop) = (recovery["inspect"].as_str().unwrap(), recovery["stop"].as_str().unwrap());
    let hint = result["error"]["hint"].as_str().unwrap();
    assert!(hint.contains(inspect) && hint.contains(stop), "{hint}");
    assert!(running.service.alive(), "GC must not stop by daemon name");

    let shell = |command: &str| {
        let out = Command::new("sh").args(["-c", command]).env("REVIEW_FIXTURE", fixture.dir.path()).output().unwrap();
        assert!(out.status.success(), "{command}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(shell(inspect).contains(&format!("\"pid\":{}", running.service.0)));
    assert!(running.service.alive(), "inspect stopped the service");
    shell(stop);
    let log = fixture.pitchfork_log();
    for action in ["status", "stop"] {
        assert!(log.lines().any(|l| l == format!("{state_dir} {action} -- {WEB_ID}")), "{action}: {log}");
    }
    assert!(!fixture.dir.path().join("pwned").exists() && !Path::new("pwned").exists(), "a recorded path was executed");
    assert!(Path::new(&pitchfork).exists());
    let deadline = Instant::now() + Duration::from_secs(5);
    while running.service.alive() {
        assert!(Instant::now() < deadline, "stop did not reach the service");
        thread::sleep(Duration::from_millis(20));
    }
    fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID, "status": "stopped" }).to_string()).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    assert_eq!(fixture.index_files(), 0);
}

#[test]
fn stale_or_reused_pids_are_never_signalled_and_ownership_is_kept() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    let other = Detached::spawn();
    for case in ["different pid", "not tracked", "stopped but recorded pid alive", "different port", "missing port", "replacement after status", "query fails"] {
        let _ = fs::remove_file(fixture.dir.path().join("pf-status.json"));
        match case {
            "different pid" => fixture.supervisor_tracks(other.0, running.port),
            "not tracked" => {}
            "stopped but recorded pid alive" => fs::write(
                fixture.dir.path().join("pf-status.json"),
                json!({ "id": WEB_ID, "status": "stopped", "pid": null }).to_string(),
            )
            .unwrap(),
            "different port" => fixture.supervisor_tracks(running.service.0, running.port.wrapping_add(1)),
            "replacement after status" => fixture.supervisor_tracks(running.service.0, running.port),
            "missing port" => fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID, "status": "running", "pid": running.service.0 }).to_string()).unwrap(),
            _ => fs::write(fixture.dir.path().join("pf-status.json"), "not json").unwrap(),
        }
        // `stop` would kill whatever the supervisor names; it must never be asked.
        fs::write(fixture.dir.path().join("pf-tracked-pid"), other.0.to_string()).unwrap();
        let (ok, result) = fixture.gc(&[]);
        assert!(!ok, "{case}: {result}");
        assert_eq!(result["error"]["code"], "gc_incomplete", "{case}: {result}");
        assert_eq!(result["error"]["details"][0]["stopped"], false, "{case}");
        assert!(running.service.alive() && other.alive(), "{case}: a process was signalled");
        assert_eq!(fixture.index_files(), 1, "{case}: ownership record dropped");
        assert!(!fixture.pitchfork_log().contains(" stop "), "{case}: {}", fixture.pitchfork_log());
        let service = &result["error"]["details"][0]["services"][0];
        // Only the recorded generation, still reported as such, may be offered for a manual stop.
        let matches = case == "replacement after status";
        assert_eq!(service["recovery"].get("stop").is_some(), matches, "{case}: {result}");
        if case != "query fails" {
            assert!(service["recovery"]["inspect"].is_string(), "{case}: {result}");
        }
    }
}

#[test]
fn a_foreign_process_on_a_dead_services_port_is_left_alone() {
    let fixture = Fixture::with_bundle(WEB);
    let mut running = fixture.start_web(&[]);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    // Our service died; some unrelated program now listens on its port.
    let port = running.port;
    drop(running.listener.take());
    drop(running.service);
    thread::sleep(Duration::from_millis(100));
    let _foreign = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    assert!(result["data"][0]["services"][0]["outcome"].as_str().unwrap().contains("left alone"), "{result}");
    assert!(!fixture.pitchfork_log().contains(" stop "));
    assert_eq!(fixture.index_files(), 0);
}

#[test]
fn a_reused_directory_inode_does_not_inherit_the_old_sessions_authority() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    let other = Detached::spawn();
    fixture.supervisor_tracks(other.0, running.port);
    for path in fixture.session_paths() {
        let mut session: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // Model a prior directory with this same device/inode, but an older birth time.
        let created = session["project_dir_created"]["secs_since_epoch"].as_u64().unwrap();
        session["project_dir_created"]["secs_since_epoch"] = json!(created - 1);
        fs::write(path, serde_json::to_vec(&session).unwrap()).unwrap();
    }
    for args in [&["up", "--json"][..], &["down", "--json"], &["status", "--json"], &["exec", "--json", "--", "true"]] {
        let out = fixture.command(args).output().unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["error"]["code"], "session_conflict", "{args:?}: {result}");
    }
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    assert_eq!(result["error"]["details"][0]["reason"], "project directory replaced", "{result}");
    assert!(other.alive() && running.service.alive());
    assert_eq!(fixture.index_files(), 1);
    assert!(!fixture.pitchfork_log().contains(" stop "));
}

#[test]
fn sessions_without_directory_birth_times_remain_readable() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    for path in fixture.session_paths() {
        let mut session: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        session.as_object_mut().unwrap().remove("project_dir_created");
        fs::write(path, serde_json::to_vec(&session).unwrap()).unwrap();
    }
    fixture.ok(&["status"]);
    fixture.ok(&["exec", "--", "true"]);
    fixture.ok(&["down"]);
}

#[test]
fn a_replaced_project_directory_does_not_inherit_the_old_sessions_authority() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&["--ttl", "1s"]);
    let app = fixture.dir.path().join("app");
    // Same path, different directory: another checkout now lives here.
    fs::remove_dir_all(&app).unwrap();
    fs::create_dir(&app).unwrap();
    fs::write(app.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
    let other = Detached::spawn();
    fixture.supervisor_tracks(other.0, running.port);
    let log = fixture.dir.path().join("mise.log");
    let _ = fs::remove_file(&log);
    thread::sleep(Duration::from_secs(2));
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    assert_eq!(result["error"]["details"][0]["reason"], "project directory replaced", "{result}");
    assert!(other.alive() && running.service.alive());
    // The new directory cannot adopt or stop the old session's services either.
    fixture.ok(&["compile"]);
    for args in [&["up", "--json"][..], &["down", "--json"], &["status", "--json"], &["exec", "--json", "--", "true"]] {
        let out = fixture.command(args).output().unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["error"]["code"], "session_conflict", "{args:?}: {result}");
    }
    assert!(other.alive() && running.service.alive());
    assert!(!fs::read_to_string(&log).unwrap_or_default().contains("daemons stop"));
    assert!(!fixture.pitchfork_log().contains(" stop "));
    // Even matching metadata cannot authorize a separate stop-by-name request.
    fixture.supervisor_tracks(running.service.0, running.port);
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    assert!(running.service.alive() && other.alive());
    assert_eq!(fixture.index_files(), 1);
}

#[test]
fn nonterminal_supervisor_states_keep_ownership_even_when_the_recorded_pid_is_dead() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    drop(running);
    thread::sleep(Duration::from_millis(100));
    for status in ["starting", "stopping", "errored", "unknown", "", "running"] {
        fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID, "status": status }).to_string()).unwrap();
        let (ok, result) = fixture.gc(&[]);
        assert!(!ok, "{status}: {result}");
        assert_eq!(fixture.index_files(), 1, "{status}");
        assert!(!fixture.pitchfork_log().contains(" stop "));
    }
    fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID }).to_string()).unwrap();
    assert!(!fixture.gc(&[]).0);
    assert_eq!(fixture.index_files(), 1);
    fs::write(fixture.dir.path().join("pf-status.json"), json!({ "id": WEB_ID, "status": "stopped" }).to_string()).unwrap();
    assert!(fixture.gc(&[]).0);
    assert_eq!(fixture.index_files(), 0);
}

#[test]
fn an_active_command_protects_a_deleted_projects_services_until_it_finishes() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fixture.supervisor_tracks(running.service.0, running.port);
    let mut child = fixture.command(&["exec", "--", "sleep", "3"]).stdout(Stdio::null()).spawn().unwrap();
    let session_file = fixture.dir.path().join("app/.stack/session.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(&session_file).is_ok_and(|s| s.contains("active_executions\": {\n    \"")) {
        assert!(Instant::now() < deadline, "execution never registered");
        thread::sleep(Duration::from_millis(10));
    }
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok && result["data"] == json!([]), "{result}");
    assert!(running.service.alive(), "stopped under an active command");
    child.wait().unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok && result["error"]["code"] == "gc_incomplete", "{result}");
    assert!(running.service.alive());
    assert_eq!(fixture.index_files(), 1);
}

#[test]
fn watch_mode_reclaims_an_expired_lease_without_another_up_but_not_a_renewed_one() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&["--ttl", "2s"]);
    // Renewed throughout: the watcher must not reclaim it.
    let watcher = fixture
        .command_at(fixture.dir.path(), &["gc", "--watch", "--interval", "1s", "--max-passes", "4", "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..8 {
        fixture.ok(&["renew"]);
        thread::sleep(Duration::from_millis(500));
    }
    let out = watcher.wait_with_output().unwrap();
    assert!(out.status.success());
    let passes: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(passes.len(), 4, "one JSON object per pass");
    assert!(passes.iter().all(|p| p["ok"] == true && p["data"]["reclaimed"] == json!([])), "{passes:?}");
    assert_eq!(fixture.index_files(), 1);

    // Left idle, it expires and is collected by the next pass, with no `stack up`.
    let mut running = running;
    drop(running.listener.take());
    fixture.supervisor_tracks(running.service.0, running.port);
    let out = fixture
        .command_at(fixture.dir.path(), &["gc", "--watch", "--interval", "1s", "--max-passes", "4", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let reclaimed: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .flat_map(|l| serde_json::from_str::<Value>(l).unwrap()["data"]["reclaimed"].as_array().unwrap().clone())
        .collect();
    assert_eq!(reclaimed.len(), 1, "{reclaimed:?}");
    assert_eq!(reclaimed[0]["stopped"], true);
    assert!(reclaimed[0]["reason"].as_str().unwrap().contains("lease expired"));
    assert_eq!(fixture.index_files(), 0);
    assert!(!running.service.alive());

    let out = fixture.command_at(fixture.dir.path(), &["gc", "--interval", "1s"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "--interval requires --watch");
}

#[test]
fn configured_home_cannot_hide_an_overlong_supervisor_socket() {
    let fixture = Fixture::with_bundle(WEB);
    let long = format!("/tmp/{}", "x".repeat(120));
    fixture.set_env(&[("HOME", long.clone())]);
    let out = fixture.command(&["up", "--json"]).env_remove("PITCHFORK_STATE_DIR").env_remove("XDG_STATE_HOME").output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "socket_path_too_long", "{result}");
    let calls = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!calls.contains("install") && !calls.contains("daemons start"), "{calls}");
    fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n[env]\nHOME={long:?}\n")).unwrap();
    let out = fixture.command(&["doctor", "--json"]).env_remove("PITCHFORK_STATE_DIR").env_remove("XDG_STATE_HOME").output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result["error"]["details"].as_array().unwrap().iter().any(|c| c["name"] == "pitchfork_socket" && c["ok"] == false), "{result}");
}

#[test]
fn every_partial_start_failure_preserves_observed_ownership() {
    for failure in ["fail-start", "fail-env-after-start", "fail-query-after-one"] {
        let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n[services.other]\nrun='false'\n");
        let inspect: Value = serde_json::from_slice(&fixture.ok(&["inspect", "--json"]).stdout).unwrap();
        let port = inspect["data"]["ports"]["web"].as_u64().unwrap() as u16;
        let other_port = inspect["data"]["ports"]["other"].as_u64().unwrap() as u16;
        let service = Detached::spawn();
        fs::write(fixture.dir.path().join("daemons-started.json"), json!([
            {"id": WEB_ID, "name": "web", "status": "running", "pid": service.0, "port": port},
            {"id": "app-0123456789abcdef/other", "name": "other", "status": "errored", "port": other_port}
        ]).to_string()).unwrap();
        fs::write(fixture.dir.path().join(failure), "").unwrap();
        let out = fixture.command(&["up", "--json"]).output().unwrap();
        assert!(!out.status.success(), "{failure}");
        for file in fixture.session_paths() {
            let record: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            assert_eq!(record["launching"], true, "{failure}: {record}");
            assert_eq!(record["services"]["web"]["pid"], service.0, "{failure}: {record}");
            assert_eq!(record["services"]["web"]["provider_id"], WEB_ID, "{failure}: {record}");
            assert_eq!(record["services"]["other"]["provider_id"], "app-0123456789abcdef/other", "{failure}: {record}");
        }
    }
}

// ---- port conflicts -------------------------------------------------------------------------

fn assigned_port(fixture: &Fixture, service: &str) -> u16 {
    let out = fixture.ok(&["inspect", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    u16::try_from(v["data"]["ports"][service].as_u64().expect("port assigned")).unwrap()
}

fn json_result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn a_foreign_listener_on_an_assigned_port_is_a_port_conflict_not_a_stop_failure() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let _squatter = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();

    let out = fixture.command(&["up", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "port_conflict", "{result}");
    assert!(result["error"]["hint"].as_str().unwrap().contains("--reassign-ports"), "{result}");
    assert_eq!(result["error"]["details"][0]["service"], "web");
    assert_eq!(result["error"]["details"][0]["port"], port);
    assert_eq!(result["error"]["details"][0]["pinned"], false);
    let steps = result["error"]["details"].as_array().unwrap().last().unwrap();
    assert_eq!(steps["changed"], false);
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("daemons stop"), "stop reconciliation ran against a foreign listener:\n{log}");
    assert!(!log.contains("daemons start"), "started over a foreign listener:\n{log}");
    assert!(!fixture.dir.path().join("app/.stack/session.json").exists(), "no ownership was recorded");

    // Stopping owns nothing here; the foreign listener is reported, not waited for.
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["confirmed"], true);
    assert_eq!(result["data"]["conflicts"][0]["port"], port);

    // Recovery: fresh ports, then a normal start on the new one.
    fixture.ok(&["compile", "--reassign-ports"]);
    let moved = assigned_port(&fixture, "web");
    assert_ne!(moved, port);
    supervise_listener(&fixture, moved);
    let result = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(result["data"]["checks"][0]["ready"], true, "{result}");
    assert_eq!(result["data"]["checks"][0]["port"], moved);
    assert_eq!(json_result(&fixture.ok(&["down", "--json"]))["data"]["confirmed"], true);
}

/// The fake supervisor starts a listener on `port` at `daemons start` and kills it at stop.
fn supervise_listener(fixture: &Fixture, port: u16) {
    fs::write(fixture.dir.path().join("listen-port"), port.to_string()).unwrap();
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "id": "app-test/web", "name": "web", "status": "running", "port": port }]).to_string(),
    )
    .unwrap();
}

/// After `compile --reassign-ports` while `web` still runs on `old`: the provider reports the
/// daemon running with its *new* configured port `new`, Pitchfork reports the actual `old`.
fn report_generation_change(fixture: &Fixture, old: u16, new: u16) {
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    fs::write(
        fixture.dir.path().join("daemons.json"),
        json!([{ "id": "app-test/web", "name": "web", "status": "running", "pid": pid, "port": new }]).to_string(),
    )
    .unwrap();
    fs::write(
        fixture.dir.path().join("pf-status.json"),
        json!({ "id": "app-test/web", "status": "running", "pid": pid, "active_port": old }).to_string(),
    )
    .unwrap();
}

#[test]
fn a_foreign_listener_on_the_new_port_of_a_changed_generation_is_a_conflict_after_the_old_stops() {
    let fixture = Fixture::with_bundle(WEB);
    let old = assigned_port(&fixture, "web");
    supervise_listener(&fixture, old);
    fixture.ok(&["up", "--json"]);
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();

    fixture.ok(&["compile", "--reassign-ports"]);
    let new = assigned_port(&fixture, "web");
    let _squatter = std::net::TcpListener::bind(("127.0.0.1", new)).unwrap();
    report_generation_change(&fixture, old, new);

    let started = Instant::now();
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "port_conflict", "{result}");
    assert!(started.elapsed() < Duration::from_secs(10), "waited on the foreign listener");
    assert_eq!(result["error"]["details"][0]["port"], new);
    let steps = result["error"]["details"].as_array().unwrap().last().unwrap()["steps"].clone();
    assert!(steps.as_array().unwrap().iter().any(|s| s["step"] == "stop_previous" && s["status"] == "ok"), "{steps}");
    // The old generation was stopped: its process is gone and its port closed.
    assert!(!pid_alive(pid), "old service survived the generation change");
    assert!(std::net::TcpStream::connect(("127.0.0.1", old)).is_err());
    let log = fs::read_to_string(fixture.dir.path().join("pitchfork.log")).unwrap();
    assert!(log.contains("status --json app-test/web"), "active port was not asked of the supervisor:\n{log}");
}

#[test]
fn down_during_a_changed_generation_stops_the_old_service_and_reports_the_new_ports_squatter() {
    let fixture = Fixture::with_bundle(WEB);
    let old = assigned_port(&fixture, "web");
    supervise_listener(&fixture, old);
    fixture.ok(&["up", "--json"]);
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    fixture.ok(&["compile", "--reassign-ports"]);
    let new = assigned_port(&fixture, "web");
    let _squatter = std::net::TcpListener::bind(("127.0.0.1", new)).unwrap();
    report_generation_change(&fixture, old, new);

    let started = Instant::now();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert!(started.elapsed() < Duration::from_secs(10), "waited on the foreign listener");
    assert_eq!(down["data"]["confirmed"], true);
    assert_eq!(down["data"]["stopped"][0]["pid"], pid);
    assert_eq!(down["data"]["conflicts"][0]["port"], new, "{down}");
    assert!(!pid_alive(pid));
}

#[test]
fn without_a_record_the_supervisors_active_port_decides_what_is_owned() {
    // No session record, the provider reports the daemon with a configured port it does not
    // listen on, and a squatter holds that configured port. Only Pitchfork's active port can
    // tell the two apart; discarding its answer would wait on the squatter.
    let fixture = Fixture::with_bundle(WEB);
    let old = assigned_port(&fixture, "web");
    supervise_listener(&fixture, old);
    fixture.ok(&["up", "--json"]);
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    for path in fixture.session_paths() {
        fs::remove_file(path).unwrap();
    }
    fixture.ok(&["compile", "--reassign-ports"]);
    let new = assigned_port(&fixture, "web");
    let _squatter = std::net::TcpListener::bind(("127.0.0.1", new)).unwrap();
    report_generation_change(&fixture, old, new);

    let started = Instant::now();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert!(started.elapsed() < Duration::from_secs(10), "waited on the squatter: {down}");
    assert_eq!(down["data"]["conflicts"][0]["port"], new, "{down}");
    assert!(!pid_alive(pid), "old service survived");
    assert!(std::net::TcpStream::connect(("127.0.0.1", old)).is_err());
    // The supervisor was discovered without a record and asked for the active port.
    let log = fs::read_to_string(fixture.dir.path().join("pitchfork.log")).unwrap();
    assert!(log.contains("status --json app-test/web"), "{log}");
    let mise_log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(mise_log.contains("which pitchfork"), "{mise_log}");

}

#[test]
fn without_a_record_or_an_active_port_the_configured_port_is_waited_for_not_assumed_foreign() {
    let fixture = Fixture::with_bundle(WEB);
    let old = assigned_port(&fixture, "web");
    supervise_listener(&fixture, old);
    fixture.ok(&["up", "--json"]);
    for path in fixture.session_paths() {
        fs::remove_file(path).unwrap();
    }
    fixture.ok(&["compile", "--reassign-ports"]);
    let new = assigned_port(&fixture, "web");
    let _squatter = std::net::TcpListener::bind(("127.0.0.1", new)).unwrap();
    report_generation_change(&fixture, old, new);
    // The supervisor knows no such daemon: nothing establishes where it listens.
    fs::remove_file(fixture.dir.path().join("pf-status.json")).unwrap();
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "stop_unconfirmed");
    assert!(std::net::TcpStream::connect(("127.0.0.1", new)).is_ok(), "the squatter was ended");
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success()
}

#[test]
fn a_pinned_port_held_by_another_program_points_at_the_override() {
    let fixture = Fixture::with_bundle(WEB);
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = squatter.local_addr().unwrap().port();
    fs::write(
        fixture.dir.path().join("app/stack.toml"),
        format!("[[use]]\nbundle='path:../bundle'\n[override.services.web]\nrun='true'\nport = {port}\n"),
    )
    .unwrap();
    fixture.ok(&["compile"]);
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "port_conflict", "{result}");
    let hint = result["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("[override.services]") && !hint.contains("--reassign-ports"), "{hint}");
    assert_eq!(result["error"]["details"][0]["pinned"], true);
}

#[test]
fn stacks_own_running_service_on_its_port_is_not_a_conflict() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    let first = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(first["data"]["checks"][0]["ready"], true, "{first}");
    // Same generation, daemon still running on the port: `up` reuses it rather than refusing.
    let second = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(second["data"]["session"]["id"], first["data"]["session"]["id"]);
    assert!(second["data"]["steps"].as_array().unwrap().iter().all(|s| s["status"] == "ok"), "{second}");

    // The supervisor refusing to stop is still a failure; a running service is never "foreign".
    fs::write(fixture.dir.path().join("fail-stop"), "").unwrap();
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "stop_failed");
    fs::remove_file(fixture.dir.path().join("fail-stop")).unwrap();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true);
    assert!(down["data"]["conflicts"].is_null(), "{down}");
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "supervised listener survived down");
}

// ---- install and logs ----------------------------------------------------------------------

#[test]
fn install_puts_locked_tools_in_place_without_starting_or_recording_anything() {
    let fixture = Fixture::with_bundle(WEB);
    let result = json_result(&fixture.ok(&["install", "--json"]));
    let steps: Vec<&str> = result["data"]["steps"].as_array().unwrap().iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["compile", "preflight", "install"], "{result}");
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(log.contains("install --yes --quiet"), "{log}");
    assert!(!log.contains("daemons start") && !log.contains("daemons stop"), "{log}");
    assert!(!fixture.dir.path().join("app/.stack/session.json").exists());
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert!(status["data"]["session"].is_null() && status["data"]["checks"][0]["ready"] == false, "{status}");

    // Locked like `up`: a request stack.lock does not pin is refused, not resolved.
    fs::write(fixture.dir.path().join("bundle/bundle.toml"), format!("{WEB}[tools]\npython='3.13'\n")).unwrap();
    let out = fixture.command(&["install", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "lock_outdated");
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("latest python"), "resolved during a locked install:\n{log}");
}

#[test]
fn logs_return_a_bounded_tail_of_what_the_supervisor_kept() {
    let fixture = Fixture::with_bundle(WEB);
    let out = fixture.command(&["logs", "web", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "logs_failed", "never started: the supervisor has nothing");
    let out = fixture.command(&["logs", "nope", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "unknown_service");

    fs::write(fixture.dir.path().join("logs.txt"), "one\ntwo\nthree\n").unwrap();
    let result = json_result(&fixture.ok(&["logs", "web", "--tail", "2", "--json"]));
    assert_eq!(result["data"]["lines"], json!(["two", "three"]));
    assert_eq!(result["data"]["truncated"], false);
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(log.contains("daemons logs -- web -n 2 --raw --no-pager"), "{log}");
    let out = fixture.ok(&["logs", "web"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\ntwo\nthree\n");
    let out = fixture.command(&["logs", "web", "--tail", "0"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn status_of_a_checkout_never_compiled_here_says_to_start_it_without_asking_the_supervisor() {
    let fixture = Fixture::with_bundle(WEB);
    // A fresh worktree has the committed stack.lock but no generated provider config, and an
    // unconfigured `mise daemons` blames its own settings; it must not be asked.
    fs::remove_file(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap();
    fs::write(fixture.dir.path().join("fail-query"), "").unwrap();
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "unhealthy");
    let result = json_result(&out);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["healthy"], false);
    assert_eq!(result["data"]["checks"][0]["ready"], false);
    assert!(result["data"]["checks"][0]["reason"].as_str().unwrap().contains("stack up"), "{result}");
    let out = fixture.command(&["logs", "web", "--json"]).output().unwrap();
    assert!(json_result(&out)["error"]["hint"].as_str().unwrap().contains("stack up"));
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("daemons --json") && !log.contains("daemons logs"), "{log}");
}

#[test]
fn a_launched_checkout_missing_its_generated_config_is_still_asked_about() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    fixture.ok(&["up"]);
    let config = fixture.dir.path().join("app/.config/mise/conf.d/stack.toml");
    fs::remove_file(&config).unwrap();
    // A launch record exists, so services may well be running: never claim otherwise.
    let result = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!(result["data"]["checks"][0]["ready"], true, "{result}");
    assert!(config.exists(), "the derived config is written again from stack.lock");
    assert_eq!(json_result(&fixture.ok(&["down", "--json"]))["data"]["confirmed"], true);
}

#[test]
fn status_reports_health_alongside_the_exit_code() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let result = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!(result["data"]["healthy"], true, "{result}");
}

#[test]
fn unknown_required_services_name_the_ones_that_exist() {
    let fixture = Fixture::with_bundle(WEB);
    let out = fixture.command(&["--json", "exec", "--require", "kafka", "--", "true"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "unknown_service");
    assert_eq!(error["hint"], "services: web");
}

#[test]
fn run_hands_a_declared_task_to_mise_without_its_daemon_startup() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.greet]\nrun='echo hi'\n[tasks.fail]\nrun='false'\n");
    let out = fixture.ok(&["run", "greet", "--", "a b", "it's", "--flag"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "greet|a b|it's|--flag|", "arguments arrive intact");
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(log.contains("run --skip-deps --no-timings greet -- a b it's --flag"), "{log}");

    let out = fixture.command(&["--json", "run", "fail"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "the task's exit code");
    assert_eq!(json_result(&out)["data"]["exit_code"], 3);

    let out = fixture.command(&["--json", "run", "nope"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "unknown_task");
    assert_eq!(error["hint"], "tasks: greet, fail");
}

#[test]
fn run_refuses_while_any_service_is_unverified() {
    // Even a task that names no services: `mise run` would hand it every service's endpoint
    // again, including the ones stack withholds.
    let fixture = Fixture::with_bundle(
        "[bundle]\nname='test'\n[services.web]\nrun='true'\n[tasks.lint]\nrun='touch ran'\n",
    );
    let out = fixture.command(&["--json", "run", "lint"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "service_unavailable", "{error}");
    assert_eq!(error["details"][0]["service"], "web");
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("run --skip-deps"), "the task must not start:\n{log}");
}

#[test]
fn mcp_runs_tasks_and_can_reassign_ports() {
    let tasks = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.hello]\nrun='echo hi'\n");
    let results = tasks.mcp(
        &[
            ("stack_run", json!({ "task": "hello", "args": ["there"] })),
            ("stack_run", json!({ "task": "nope" })),
        ],
        &[],
    );
    assert_eq!(results[0]["structuredContent"]["data"]["stdout"], "hello|there|", "{}", results[0]);
    assert_eq!(results[1]["structuredContent"]["error"]["code"], "unknown_task");

    let fixture = Fixture::with_bundle(WEB);
    let before = json_result(&fixture.ok(&["inspect", "--json"]))["data"]["ports"]["web"].clone();
    // Another program takes the port, as in a port_conflict.
    let _held = std::net::TcpListener::bind(("127.0.0.1", before.as_u64().unwrap() as u16)).unwrap();
    let results = fixture.mcp(&[("stack_compile", json!({ "reassign_ports": true }))], &[]);
    let after = &results[0]["structuredContent"]["data"]["ports"]["web"];
    assert!(after.is_u64() && *after != before, "{before} -> {after}");
}

#[test]
fn interrupting_up_stops_its_start_request_so_nothing_launches_after_down() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::with_bundle(WEB);
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    fs::write(fixture.dir.path().join("late-client"), "").unwrap();
    // Like Ctrl-C in a terminal: the signal goes to stack's foreground process group.
    let mut up = fixture.command(&["up", "--json"]).process_group(0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let waiting = fixture.dir.path().join("client-waiting");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiting.exists() {
        assert!(Instant::now() < deadline, "the start request's client never started");
        thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::kill(-(up.id() as i32), libc::SIGINT) };
    assert!(!up.wait().unwrap().success());
    fixture.ok(&["down"]);
    thread::sleep(Duration::from_secs(3));
    assert!(!fixture.dir.path().join("late-launch").exists(), "a client of the interrupted start acted after down");
}

#[test]
fn an_ignored_hangup_stays_ignored_after_captured_commands() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::new();
    let mut command = fixture.command(&["mcp"]);
    // As under `nohup`: SIGHUP is ignored when stack starts.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut server = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let mut lines = BufReader::new(server.stdout.take().unwrap()).lines();
    let app = fixture.dir.path().join("app");
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["true"],"dir":app}}});
    writeln!(stdin, "{call}").unwrap();
    assert!(lines.next().unwrap().unwrap().contains("\"isError\":false"), "a captured command ran");
    unsafe { libc::kill(server.id() as i32, libc::SIGHUP) };
    thread::sleep(Duration::from_millis(200));
    writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":2,"method":"ping"})).unwrap();
    let reply: Value = serde_json::from_str(&lines.next().expect("server still answering").unwrap()).unwrap();
    assert_eq!(reply["id"], 2);
    drop(stdin);
    assert!(server.wait().unwrap().success());
}

/// `web` as the supervisor reports it once started: running as `service`, which never listens.
fn supervise_never_ready(fixture: &Fixture, service: &Detached) {
    let port = assigned_port(fixture, "web");
    fs::write(fixture.dir.path().join("pf-tracked-pid"), service.0.to_string()).unwrap();
    fs::write(
        fixture.dir.path().join("daemons-started.json"),
        json!([{ "id": "app-test/web", "name": "web", "status": "running", "pid": service.0, "port": port }]).to_string(),
    )
    .unwrap();
}

fn launch_records(fixture: &Fixture) -> Vec<Value> {
    fixture.session_paths().iter().map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap()).collect()
}

#[test]
fn up_gives_up_at_its_timeout_and_keeps_a_never_ready_service_recorded_until_down() {
    let fixture = Fixture::with_bundle(WEB);
    let service = Detached::spawn();
    supervise_never_ready(&fixture, &service);
    let start = Instant::now();
    let out = fixture.command(&["up", "--timeout", "2s", "--json"]).output().unwrap();
    let took = start.elapsed();
    // Readiness alone would have been waited for 90s.
    assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(20), "{took:?}");
    assert_eq!(out.status.code(), Some(124));
    let result = json_result(&out);
    let error = &result["error"];
    assert_eq!(error["code"], "timed_out", "{result}");
    assert_eq!(error["message"], "`stack up` did not finish within 2s");
    assert!(error["hint"].as_str().unwrap().contains("`stack down` stops them"), "{result}");
    assert_eq!(error["details"][0]["cause"]["code"], "not_ready", "{result}");
    assert_eq!(error["details"][0]["cause"]["details"][0]["service"], "web", "{result}");
    let progress = &error["details"][1];
    assert_eq!(progress["changed"], true, "{result}");
    let steps: Vec<(&str, &str)> = progress["steps"].as_array().unwrap().iter().map(|s| (s["step"].as_str().unwrap(), s["status"].as_str().unwrap())).collect();
    assert_eq!(&steps[steps.len() - 2..], [("start", "ok"), ("verify", "failed")], "{result}");

    // Nothing stopped the service, and its ownership is recorded as an incomplete launch.
    assert!(service.alive());
    for record in launch_records(&fixture) {
        assert_eq!(record["launching"], true, "{record}");
        assert_eq!(record["services"]["web"]["pid"], service.0, "{record}");
    }
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["healthy"], false, "{status}");
    assert!(!fixture.command(&["exec", "--require", "web", "--", "true"]).output().unwrap().status.success());

    // `down` stops it through the supervisor, and nothing is left for GC.
    fixture.ok(&["down"]);
    assert!(!service.alive());
    assert_eq!(json_result(&fixture.ok(&["gc", "--json"]))["data"], json!([]));

    // Once the service can become ready, the same command succeeds.
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up", "--timeout", "30s"]);
    fixture.ok(&["exec", "--require", "web", "--", "true"]);
    fixture.ok(&["down"]);
}

#[test]
fn mcp_up_reports_its_timeout_as_an_error_and_down_still_stops_what_launched() {
    let fixture = Fixture::with_bundle(WEB);
    let service = Detached::spawn();
    supervise_never_ready(&fixture, &service);
    let results = fixture.mcp(&[("stack_up", json!({ "timeout_secs": 1 })), ("stack_status", json!({})), ("stack_down", json!({}))], &[]);
    assert_eq!(results[0]["isError"], true, "{}", results[0]);
    let error = &results[0]["structuredContent"]["error"];
    assert_eq!(error["code"], "timed_out", "{error}");
    assert_eq!(error["message"], "`stack up` did not finish within 1s");
    assert_eq!(error["details"][0]["cause"]["code"], "not_ready", "{error}");
    assert_eq!(results[1]["structuredContent"]["data"]["healthy"], false, "{}", results[1]);
    assert_eq!(results[2]["isError"], false, "{}", results[2]);
    assert!(!service.alive());
}

#[test]
fn a_start_request_cut_short_by_the_timeout_is_killed_with_its_process_group() {
    let fixture = Fixture::with_bundle(WEB);
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    fs::write(fixture.dir.path().join("late-client"), "").unwrap();
    let start = Instant::now();
    let out = fixture.command(&["up", "--timeout", "2s"]).output().unwrap();
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
    assert_eq!(out.status.code(), Some(124));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[timed_out]: `stack up` did not finish within 2s"), "{stderr}");
    assert!(stderr.contains("cut short: error[start_failed]"), "{stderr}");
    assert!(fixture.dir.path().join("client-waiting").exists(), "the start request never ran");
    // What the supervisor reported after the cut-short start is recorded, not forgotten.
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    let after_start = log.split("daemons start").nth(1).expect("start requested");
    assert!(after_start.contains("daemons --json"), "{log}");
    for record in launch_records(&fixture) {
        assert_eq!(record["launching"], true, "{record}");
    }
    // The request's own descendants went with it: none acts after the timeout.
    thread::sleep(Duration::from_secs(3));
    assert!(!fixture.dir.path().join("late-launch").exists(), "a client of the cut-short start acted later");
    fs::remove_file(fixture.dir.path().join("slow-start")).unwrap();
    fixture.ok(&["down"]);
}

#[test]
fn startup_waits_for_a_busy_project_only_until_its_timeout() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::with_bundle(WEB);
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    fs::write(fixture.dir.path().join("late-client"), "").unwrap();
    let mut first = fixture.command(&["up", "--timeout", "60s"]).process_group(0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let waiting = fixture.dir.path().join("client-waiting");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiting.exists() {
        assert!(Instant::now() < deadline, "the first start request never started");
        thread::sleep(Duration::from_millis(20));
    }

    let start = Instant::now();
    let out = fixture.command(&["restart", "--timeout", "1s", "--json"]).output().unwrap();
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(out.status.code(), Some(124));
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    assert_eq!(result["error"]["details"][0]["cause"]["code"], "lock_busy", "{result}");
    assert!(result["error"]["hint"].as_str().unwrap().starts_with("nothing was started"), "{result}");
    let results = fixture.mcp(&[("stack_up", json!({ "timeout_secs": 1 }))], &[]);
    assert_eq!(results[0]["isError"], true, "{}", results[0]);
    assert_eq!(results[0]["structuredContent"]["error"]["details"][0]["cause"]["code"], "lock_busy", "{}", results[0]);

    unsafe { libc::kill(-(first.id() as i32), libc::SIGINT) };
    assert!(!first.wait().unwrap().success());
    fs::remove_file(fixture.dir.path().join("slow-start")).unwrap();
    fixture.ok(&["down"]);
}

#[test]
fn a_restart_cut_short_keeps_the_session_and_records_the_incomplete_launch() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    let start = Instant::now();
    let out = fixture.command(&["restart", "web", "--timeout", "2s", "--json"]).output().unwrap();
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
    assert_eq!(out.status.code(), Some(124));
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    assert_eq!(result["error"]["message"], "`stack restart` did not finish within 2s");
    assert_eq!(result["error"]["details"][1]["changed"], true, "{result}");
    for record in launch_records(&fixture) {
        assert_eq!(record["id"], up["data"]["session"]["id"], "{record}");
        assert_eq!(record["launching"], true, "{record}");
    }
    assert!(!fixture.command(&["exec", "--require", "web", "--", "true"]).output().unwrap().status.success());

    // A retried `up` completes the launch; `down` then confirms cleanup.
    fs::remove_file(fixture.dir.path().join("slow-start")).unwrap();
    fixture.ok(&["up", "--timeout", "30s"]);
    fixture.ok(&["exec", "--require", "web", "--", "true"]);
    fixture.ok(&["down"]);
}

#[test]
fn startup_timeouts_are_checked_before_any_lifecycle_work() {
    let fixture = Fixture::with_bundle(WEB);
    for args in [&["up", "--timeout", "0s"][..], &["up", "--timeout", "soon"], &["restart", "web", "--timeout", "0"]] {
        let out = fixture.command(&[&["--json"][..], args].concat()).output().unwrap();
        assert!(!out.status.success() && out.status.code() != Some(124), "{args:?}");
        assert_eq!(json_result(&out)["error"]["code"], "usage", "{args:?}");
    }
    let results = fixture.mcp(
        &[
            ("stack_up", json!({ "timeout_secs": 0 })),
            ("stack_up", json!({ "timeout_secs": "30" })),
            ("stack_restart", json!({ "services": ["web"], "timeout_secs": 1.5 })),
        ],
        &[],
    );
    for result in &results {
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(result["structuredContent"]["error"]["code"], "usage", "{result}");
    }
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("daemons"), "{log}");
    assert!(!fixture.dir.path().join("app/.stack/session.json").exists());
    assert_eq!(fixture.index_files(), 0);
}
