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
    cat "$REVIEW_FIXTURE/daemons.json" ;;
  'daemons start')
    if test -f "$REVIEW_FIXTURE/slow-start"; then sleep "$(cat "$REVIEW_FIXTURE/slow-start")"; fi
    if test -f "$REVIEW_FIXTURE/daemons-started.json"; then
      cp "$REVIEW_FIXTURE/daemons-started.json" "$REVIEW_FIXTURE/daemons.json"
      touch "$REVIEW_FIXTURE/started"
    fi
    if test -f "$REVIEW_FIXTURE/fail-start"; then echo 'start failed' >&2; exit 1; fi ;;
  'daemons stop')
    if test -f "$REVIEW_FIXTURE/fail-stop"; then echo 'cannot stop' >&2; exit 1; fi
    if test -f "$REVIEW_FIXTURE/pf-tracked-pid"; then kill "$(cat "$REVIEW_FIXTURE/pf-tracked-pid")" 2>/dev/null; fi ;;
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

    let out = fixture
        .command(&["--json", "exec", "--timeout", "1s", "--", "sleep", "10"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"]["timed_out"], true);

    let out = fixture.command(&["exec", "--timeout", "1s", "--", "true"]).output().unwrap();
    assert!(!out.status.success(), "--timeout without --json is rejected");
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
fn a_deleted_projects_services_are_stopped_through_the_supervisor() {
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
    assert!(ok, "{result}");
    let entry = &result["data"][0];
    assert_eq!(entry["reason"], "project directory deleted");
    assert_eq!(entry["stopped"], true, "{result}");
    assert!(entry["services"][0]["outcome"].as_str().unwrap().contains("stopped pid"), "{result}");
    assert!(!running.service.alive(), "the owned service survived");
    assert_eq!(fixture.index_files(), 0, "ownership record released once confirmed");
    assert!(!app.exists(), "the deleted project was recreated");
    let log = fixture.pitchfork_log();
    assert!(log.contains(&format!("{state_dir} status --json {WEB_ID}")), "{log}");
    assert!(log.contains(&format!("{state_dir} stop {WEB_ID}")), "{log}");
    assert_eq!(fixture.gc(&[]).1["data"], json!([]), "nothing left to reclaim");
}

#[test]
fn stale_or_reused_pids_are_never_signalled_and_ownership_is_kept() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    let other = Detached::spawn();
    for case in ["different pid", "not tracked", "stopped but recorded pid alive", "different port", "query fails"] {
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
    // Once the supervisor confirms the recorded process, gc reclaims it and the path is free.
    fixture.supervisor_tracks(running.service.0, running.port);
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    assert!(!running.service.alive() && other.alive());
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((status["ok"].clone(), status["data"]["session"].clone()), (json!(true), Value::Null), "{status}");
}

#[test]
fn a_failed_supervisor_stop_keeps_ownership_for_a_retry() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fixture.supervisor_tracks(running.service.0, running.port);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    fs::write(fixture.dir.path().join("pf-fail-stop"), "").unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(!ok);
    assert!(result["error"]["details"][0]["error"].as_str().unwrap().contains("cannot stop"), "{result}");
    assert_eq!(fixture.index_files(), 1);
    fs::remove_file(fixture.dir.path().join("pf-fail-stop")).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    assert!(!running.service.alive());
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
    assert!(ok && result["data"][0]["stopped"] == true, "{result}");
    assert!(!running.service.alive());
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
