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
  'env --json') cat "$REVIEW_FIXTURE/env.json" ;;
  'daemons --json')
    if test -f "$REVIEW_FIXTURE/fail-query"; then echo 'supervisor unavailable' >&2; exit 1; fi
    cat "$REVIEW_FIXTURE/daemons.json" ;;
  'daemons start')
    if test -f "$REVIEW_FIXTURE/daemons-started.json"; then
      cp "$REVIEW_FIXTURE/daemons-started.json" "$REVIEW_FIXTURE/daemons.json"
      touch "$REVIEW_FIXTURE/started"
    fi ;;
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

#[test]
fn container_mcp_scenario_rejects_a_server_that_only_returns_an_error() {
    let fixture = Fixture::new();
    let prefix = format!("{}/", fixture.dir.path().display());
    let helper = include_str!("e2e/assert.sh").replace("/tmp/", &prefix);
    fs::write(fixture.dir.path().join("stack-e2e-assert.sh"), helper).unwrap();
    let fake = fixture.dir.path().join("bin/stack");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"structuredContent\":{\"ok\":false,\"error\":{\"code\":\"exec_failed\"}}}}'\nexit 1\n").unwrap();
    fs::set_permissions(fake, fs::Permissions::from_mode(0o755)).unwrap();
    let script = include_str!("e2e/4-mcp.sh")
        .replace("/tmp/", &prefix)
        .replace(
            "cd ~/appA",
            &format!("cd '{}'", fixture.dir.path().display()),
        )
        .replace("export PATH=/opt/stack:$PATH", "");
    let out = Command::new("bash")
        .args(["-c", &script])
        .env(
            "PATH",
            format!(
                "{}:{}",
                fixture.dir.path().join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
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
