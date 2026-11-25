#![cfg(unix)]
//! Project discovery and the other CLI ergonomics, against the fake mise in tests/fakes.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(bundle: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bin", "bundle", "app/src/deep"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        // The checkout is its own Git worktree, so discovery stops at `app`.
        fs::create_dir(dir.path().join("app/.git")).unwrap();
        fs::write(dir.path().join("bundle/bundle.toml"), bundle).unwrap();
        fs::write(dir.path().join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
        fs::write(dir.path().join("daemons.json"), "[]").unwrap();
        let path = format!("{}:{}", dir.path().join("bin").display(), std::env::var("PATH").unwrap());
        fs::write(dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
        let mise = dir.path().join("bin/mise");
        fs::write(&mise, FAKE_MISE).unwrap();
        fs::set_permissions(mise, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self { dir };
        fixture.ok(&["-C", fixture.app().to_str().unwrap(), "compile"]);
        fixture
    }

    fn app(&self) -> PathBuf {
        self.dir.path().join("app").canonicalize().unwrap()
    }

    /// Append `text`'s lines to what Stack's supervisor kept of `service`'s output, stamped now.
    fn logs(&self, service: &str, text: &str) {
        let dir = self.dir.path().join("state/supervisors").join(stack::state::project_key(&self.app())).join("logs");
        fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let stamped: String = text.lines().map(|l| format!("{now} {l}\n")).collect();
        let mut file = fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{service}.log"))).unwrap();
        std::io::Write::write_all(&mut file, stamped.as_bytes()).unwrap();
    }

    /// `stack <args>` run from `cwd`, without `-C` unless `args` gives one.
    fn command_in(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .current_dir(cwd)
            .args(args)
            .env("PATH", format!("{}:{}", self.dir.path().join("bin").display(), std::env::var("PATH").unwrap()))
            .env("REVIEW_FIXTURE", self.dir.path())
            .env("REVIEW_PYTHON", python::python())
            .env("STACK_STATE_DIR", self.dir.path().join("state"))
            .env("STACK_CACHE_DIR", self.dir.path().join("cache"))
            .env("PITCHFORK_STATE_DIR", self.dir.path().join("pfs"))
            .stdin(Stdio::null());
        command
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let out = self.command_in(cwd, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    fn ok(&self, args: &[&str]) -> Output {
        self.ok_in(&self.app(), args)
    }

    /// Answers to `calls` made over one MCP session started in `cwd`, in call order.
    fn mcp(&self, cwd: &Path, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut input = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string() + "\n";
        for (i, (name, args)) in calls.iter().enumerate() {
            input += &(json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string() + "\n");
        }
        let mut child = self.command_in(cwd, &["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        // The server answers calls on different checkouts concurrently, so replies are matched by id.
        let mut answered: Vec<Value> = stdout(&out)
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|m| m["id"].as_u64().is_some_and(|id| id > 0))
            .collect();
        answered.sort_by_key(|m| m["id"].as_u64());
        answered.iter().map(|m| m["result"]["structuredContent"].clone()).collect()
    }
}

fn json_result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_subdirectory_finds_its_project_and_exec_runs_where_it_was_called() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let deep = fixture.app().join("src/deep");
    let inspect = json_result(&fixture.ok_in(&deep, &["inspect", "--json"]));
    assert_eq!(inspect["ok"], true, "{inspect}");
    // The child runs in the caller's directory, and STACK_PROJECT names the project.
    let out = fixture.ok_in(&deep, &["exec", "--", "sh", "-c", "pwd -P; echo $STACK_PROJECT"]);
    assert_eq!(stdout(&out), format!("{}\n{}\n", deep.display(), fixture.app().display()));
    let out = fixture.ok_in(&deep, &["--json", "exec", "--", "pwd", "-P"]);
    assert_eq!(json_result(&out)["data"]["stdout"], format!("{}\n", deep.display()));
    // An explicit -C keeps the project root as the working directory.
    let out = fixture.ok_in(&deep, &["-C", fixture.app().to_str().unwrap(), "exec", "--", "pwd", "-P"]);
    assert_eq!(stdout(&out), format!("{}\n", fixture.app().display()));
}

#[test]
fn without_a_manifest_the_error_says_where_it_looked_and_points_at_init() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    fs::remove_file(fixture.app().join("stack.toml")).unwrap();
    let deep = fixture.app().join("src/deep");
    let out = fixture.command_in(&deep, &["inspect", "--json"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "manifest_missing", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(&deep.display().to_string()) && message.contains(&format!("up to {}", fixture.app().display())), "{message}");
    assert!(error["hint"].as_str().unwrap().contains("stack init"), "{error}");
    // A stack.toml above the worktree root is not this checkout's.
    fs::write(fixture.dir.path().join("stack.toml"), "").unwrap();
    let out = fixture.command_in(&deep, &["inspect", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "manifest_missing");
    // `-C` means exactly that directory: no search, and the plain error.
    let out = fixture.command_in(fixture.dir.path(), &["-C", deep.to_str().unwrap(), "inspect", "--json"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["code"], "manifest_missing", "{error}");
    assert!(!error["message"].as_str().unwrap().contains("up to"), "{error}");
}

#[test]
fn init_from_a_subdirectory_of_a_project_refuses_to_write_a_second_one() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let deep = fixture.app().join("src/deep");
    let out = fixture.command_in(&deep, &["init", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "project_exists");
    assert!(!deep.join("stack.toml").exists());
}

#[test]
fn mcp_dir_finds_its_project_like_the_cli_and_never_uses_the_servers_own() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let app = fixture.app();
    let deep = app.join("src/deep");
    fixture.ok(&["compile"]);
    // Started inside the checkout: the server's working directory still decides nothing.
    let replies = fixture.mcp(
        &deep,
        &[
            ("stack_inspect", json!({ "dir": app })),
            ("stack_inspect", json!({ "dir": deep })),
            ("stack_inspect", json!({})),
            ("stack_exec", json!({ "dir": deep, "command": ["sh", "-c", "pwd -P; echo $STACK_PROJECT"] })),
        ],
    );
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    // A directory inside the checkout finds its stack.toml, as the CLI does from there.
    assert_eq!(replies[1]["ok"], true, "{}", replies[1]);
    assert_eq!(replies[1]["project"], app.to_str().unwrap());
    assert_eq!(replies[2]["error"]["code"], "usage", "{}", replies[2]);
    // The command runs where `dir` points, like `stack exec` from a subdirectory.
    assert_eq!(replies[3]["data"]["stdout"], format!("{}\n{}\n", deep.display(), app.display()), "{}", replies[3]);
    assert_eq!(replies[3]["project"], app.to_str().unwrap());
    // stack_init means exactly `dir`: a new project inside this one, never this one's stack.toml.
    // Sent after stack_exec so concurrent MCP dispatch (keyed on `app` vs `deep`) cannot create
    // `deep/stack.toml` before stack_exec resolves its project.
    let init = fixture.mcp(&deep, &[("stack_init", json!({ "dir": deep }))]);
    assert_eq!(init[0]["ok"], true, "{}", init[0]);
    assert_eq!(init[0]["project"], deep.to_str().unwrap());
    assert!(deep.join("stack.toml").is_file());
    // With no stack.toml up to the worktree root, the error says where the search went.
    fs::remove_file(deep.join("stack.toml")).unwrap();
    fs::remove_file(app.join("stack.toml")).unwrap();
    let replies = fixture.mcp(&app, &[("stack_inspect", json!({ "dir": deep }))]);
    assert_eq!(replies[0]["error"]["code"], "manifest_missing", "{}", replies[0]);
    assert_eq!(replies[0]["project"], deep.to_str().unwrap());
    assert!(replies[0]["error"]["message"].as_str().unwrap().contains(&format!("up to {}", app.display())), "{}", replies[0]);
}

#[test]
fn lifecycle_progress_stays_off_pipes() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    for args in [&["install"][..], &["compile"], &["--quiet", "install"], &["-q", "compile"]] {
        let out = fixture.ok(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("stack: install") && !stderr.contains("stack: compile"), "{args:?}: {stderr}");
    }
}

const TWO: &str = "[bundle]\nname='test'\n[services.web]\nrun='true'\n[services.worker]\nrun='true'\n";

#[test]
fn logs_of_every_service_are_prefixed_filtered_and_split_per_service() {
    let fixture = Fixture::new(TWO);
    fixture.logs("web", "web up\nGET / 200\nGET /x 500 Error\n");
    fixture.logs("worker", "worker up\nerror: retrying\n");
    let out = fixture.ok(&["logs"]);
    assert_eq!(stdout(&out), "web    | web up\nweb    | GET / 200\nweb    | GET /x 500 Error\nworker | worker up\nworker | error: retrying\n");
    let result = json_result(&fixture.ok(&["logs", "--json", "--grep", "error", "-i"]));
    assert_eq!(result["data"]["services"]["web"]["lines"], json!(["GET /x 500 Error"]), "{result}");
    assert_eq!(result["data"]["services"]["worker"]["lines"], json!(["error: retrying"]));
    // One named service keeps the single-service shape; --tail counts matching lines.
    let result = json_result(&fixture.ok(&["logs", "web", "--json", "--grep", "GET", "--tail", "1"]));
    assert_eq!(result["data"]["lines"], json!(["GET /x 500 Error"]), "{result}");
    // --since keeps that window: everything here was written just now.
    let result = json_result(&fixture.ok(&["logs", "web", "--since", "10m", "--json"]));
    assert_eq!(result["data"]["lines"].as_array().unwrap().len(), 3, "{result}");
    // Nothing is asked of a provider process.
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap_or_default();
    assert!(!log.contains("daemons"), "{log}");

    let out = fixture.command_in(&fixture.app(), &["logs", "nope", "worker", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "unknown_service");
    let out = fixture.command_in(&fixture.app(), &["logs", "--ignore-case"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "-i needs --grep");
}

#[test]
fn mcp_logs_of_every_service_filtered() {
    let fixture = Fixture::new(TWO);
    fixture.logs("web", "a\nboom\n");
    fixture.logs("worker", "b\n");
    let app = fixture.app();
    let replies = fixture.mcp(&app, &[("stack_logs", json!({ "dir": app, "grep": "BOOM", "ignore_case": true })), ("stack_logs", json!({ "dir": app, "service": "worker" }))]);
    assert_eq!(replies[0]["data"]["services"]["web"]["lines"], json!(["boom"]), "{}", replies[0]);
    assert_eq!(replies[0]["data"]["services"]["worker"]["lines"], json!([]));
    assert_eq!(replies[1]["data"]["lines"], json!(["b"]), "{}", replies[1]);
}

impl Fixture {
    fn log_file(&self, service: &str) -> PathBuf {
        self.dir.path().join("state/supervisors").join(stack::state::project_key(&self.app())).join("logs").join(format!("{service}.log"))
    }

    fn logs_json(&self, args: &[&str]) -> Value {
        let mut all = vec!["logs", "--json"];
        all.extend_from_slice(args);
        let r = json_result(&self.ok(&all));
        assert_eq!(r["ok"], true, "{r}");
        r["data"].clone()
    }
}

#[test]
fn a_log_cursor_returns_each_line_once_with_its_time_and_says_what_it_missed() {
    let fixture = Fixture::new(TWO);
    let log = fixture.log_file("web");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    // The current writer starts every new log with its own generation, even on a reused inode.
    let begin_generation = |generation: u128| fs::write(&log, format!("# stack log {generation:032x}\n")).unwrap();
    begin_generation(1);
    fixture.logs("web", "one\n");
    // Without a cursor: as before, plus where the read got to.
    let first = fixture.logs_json(&["web"]);
    assert_eq!(first["lines"], json!(["one"]), "{first}");
    assert!(first.get("timestamps").is_none(), "existing clients see what they saw: {first}");
    let cursor = first["cursor"].as_str().unwrap().to_string();
    // Repeated lines are each returned, once, with when they were read.
    fixture.logs("web", "same\nsame\n");
    let next = fixture.logs_json(&["web", "--cursor", &cursor]);
    assert_eq!(next["lines"], json!(["same", "same"]), "{next}");
    let stamps = next["timestamps"].as_array().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    assert!(stamps.len() == 2 && stamps.iter().all(|t| t.as_u64().unwrap() <= now && t.as_u64().unwrap() + 60_000 > now), "{next}");
    let cursor = next["cursor"].as_str().unwrap().to_string();
    let none = fixture.logs_json(&["web", "--cursor", &cursor]);
    assert_eq!(none["lines"], json!([]), "{none}");
    assert!(none.get("gap").is_none() && none.get("more").is_none(), "{none}");
    // A record still being written waits for its newline.
    let mut file = fs::OpenOptions::new().append(true).open(fixture.log_file("web")).unwrap();
    std::io::Write::write_all(&mut file, format!("{now} half").as_bytes()).unwrap();
    assert_eq!(fixture.logs_json(&["web", "--cursor", &cursor])["lines"], json!([]));
    std::io::Write::write_all(&mut file, b" done\n").unwrap();
    let whole = fixture.logs_json(&["web", "--cursor", &cursor]);
    assert_eq!(whole["lines"], json!(["half done"]), "{whole}");
    let cursor = whole["cursor"].as_str().unwrap().to_string();
    // --tail caps a read; `more` says to read again.
    fixture.logs("web", "a\nb\nc\n");
    let capped = fixture.logs_json(&["web", "--tail", "2", "--cursor", &cursor]);
    assert_eq!((capped["lines"].clone(), capped["more"].clone()), (json!(["a", "b"]), json!(true)), "{capped}");
    let rest = fixture.logs_json(&["web", "--tail", "2", "--cursor", capped["cursor"].as_str().unwrap()]);
    assert_eq!(rest["lines"], json!(["c"]), "{rest}");
    let cursor = rest["cursor"].as_str().unwrap().to_string();
    // Rotated once: followed into the new file. Twice: the loss is said.
    let older = fixture.log_file("web").with_extension("log.1");
    fixture.logs("web", "before rotation\n");
    fs::rename(fixture.log_file("web"), &older).unwrap();
    begin_generation(2);
    fixture.logs("web", "after rotation\n");
    let rotated = fixture.logs_json(&["web", "--cursor", &cursor]);
    assert_eq!(rotated["lines"], json!(["before rotation", "after rotation"]), "{rotated}");
    assert!(rotated.get("gap").is_none(), "{rotated}");
    fs::rename(fixture.log_file("web"), &older).unwrap();
    begin_generation(3);
    fixture.logs("web", "x\n");
    fs::rename(fixture.log_file("web"), &older).unwrap();
    begin_generation(4);
    fixture.logs("web", "y\n");
    let lost = fixture.logs_json(&["web", "--cursor", rotated["cursor"].as_str().unwrap()]);
    assert_eq!((lost["lines"].clone(), lost["gap"].clone()), (json!(["x", "y"]), json!("rotated")), "{lost}");

    // A cursor read compiles nothing: it works while stack.toml cannot be compiled.
    let cursor = lost["cursor"].as_str().unwrap().to_string();
    fixture.logs("web", "z\n");
    fs::write(fixture.app().join("stack.toml"), "this is not toml [[[").unwrap();
    assert_eq!(fixture.logs_json(&["web", "--cursor", &cursor])["lines"], json!(["z"]));
    let out = fixture.command_in(&fixture.app(), &["logs", "web", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["ok"], false, "a read without a cursor still checks the project");
    let out = fixture.command_in(&fixture.app(), &["logs", "web", "--json", "--cursor", "nonsense"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "usage");
}

#[test]
fn a_service_with_no_log_yet_stays_in_a_cursor_of_several() {
    let fixture = Fixture::new(TWO);
    fixture.logs("web", "w1\n");
    let begun = fixture.logs_json(&["--cursor", ""]);
    assert_eq!(begun["services"]["web"]["lines"], json!(["w1"]), "{begun}");
    assert_eq!(begun["errors"]["worker"]["code"], "logs_failed", "{begun}");
    let cursor = begun["cursor"].as_str().unwrap().to_string();
    let quiet = fixture.logs_json(&["--cursor", &cursor]);
    assert_eq!(quiet["services"]["worker"]["lines"], json!([]), "named by the cursor, read without a log: {quiet}");
    let cursor = quiet["cursor"].as_str().unwrap().to_string();
    fixture.logs("worker", "k1\n");
    fixture.logs("web", "w2\n");
    let later = fixture.logs_json(&["--cursor", &cursor]);
    assert_eq!((later["services"]["web"]["lines"].clone(), later["services"]["worker"]["lines"].clone()), (json!(["w2"]), json!(["k1"])), "{later}");
    let last = fixture.logs_json(&["--cursor", later["cursor"].as_str().unwrap()]);
    assert_eq!((last["services"]["web"]["lines"].clone(), last["services"]["worker"]["lines"].clone()), (json!([]), json!([])), "{last}");
}

#[test]
fn mcp_logs_read_from_a_cursor_across_services() {
    let fixture = Fixture::new(TWO);
    fixture.logs("web", "w1\n");
    fixture.logs("worker", "k1\n");
    let app = fixture.app();
    let begun = fixture.mcp(&app, &[("stack_logs", json!({ "dir": app, "cursor": "" }))]);
    let data = &begun[0]["data"];
    assert_eq!(data["services"]["web"]["lines"], json!(["w1"]), "{}", begun[0]);
    assert_eq!(data["services"]["web"]["timestamps"].as_array().unwrap().len(), 1, "{}", begun[0]);
    let cursor = data["cursor"].as_str().unwrap().to_string();
    fixture.logs("worker", "k2\nk2\n");
    let next = fixture.mcp(&app, &[
        ("stack_logs", json!({ "dir": app, "cursor": cursor })),
        ("stack_logs", json!({ "dir": app, "cursor": cursor, "service": "worker", "grep": "K2", "ignore_case": true, "tail": 1 })),
        ("stack_logs", json!({ "dir": app, "cursor": 7 })),
        ("stack_logs", json!({ "dir": app, "cursor": cursor, "project": true })),
    ]);
    assert_eq!(next[0]["data"]["services"]["web"]["lines"], json!([]), "{}", next[0]);
    assert_eq!(next[0]["data"]["services"]["worker"]["lines"], json!(["k2", "k2"]), "{}", next[0]);
    assert_eq!((next[1]["data"]["lines"].clone(), next[1]["data"]["more"].clone()), (json!(["k2"]), json!(true)), "{}", next[1]);
    assert_eq!(next[2]["error"]["code"], "usage", "{}", next[2]);
    assert_eq!(next[3]["error"]["code"], "usage", "{}", next[3]);
}

#[test]
fn follow_prints_new_lines_until_interrupted() {
    let fixture = Fixture::new(TWO);
    fixture.logs("web", "one\n");
    fixture.logs("worker", "");
    let child = fixture.command_in(&fixture.app(), &["logs", "web", "--follow"]).stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fixture.logs("web", "two\n");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // The same line again in a later poll: printed again (no window is matched up).
    fixture.logs("web", "two\n");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    // SAFETY: signals a child this test started and has not reaped.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    let out = child.wait_with_output().unwrap();
    // Like `tail -f`: the interrupt ends it, after every line read so far was written.
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(libc::SIGINT), "{:?}", out.status);
    assert_eq!(stdout(&out), "one\ntwo\ntwo\n");

    let out = fixture.command_in(&fixture.app(), &["logs", "--follow", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "usage");
}

#[test]
fn env_withholds_exactly_what_exec_withholds() {
    let fixture = Fixture::new(TWO);
    let inspect = json_result(&fixture.ok(&["inspect", "--json"]));
    let port = inspect["data"]["ports"]["web"].as_u64().unwrap();
    let path = format!("{}:{}", fixture.dir.path().join("bin").display(), std::env::var("PATH").unwrap());
    fs::write(
        fixture.dir.path().join("env.json"),
        json!({ "PATH": path, "WEB_URL": format!("http://127.0.0.1:{port}/"), "WEB_PORT": port.to_string(), "PLAIN": "it's" }).to_string(),
    )
    .unwrap();
    fixture.ok(&["install"]);
    let exec = stdout(&fixture.ok(&["exec", "--", "sh", "-c", "echo \"$WEB_URL|${WEB_PORT-ABSENT}|$STACK_UNVERIFIED\""]));
    let env = json_result(&fixture.ok(&["env", "--json"]));
    let data = &env["data"];
    assert_eq!(exec, format!("{}|ABSENT|{}\n", data["env"]["WEB_URL"].as_str().unwrap(), data["env"]["STACK_UNVERIFIED"].as_str().unwrap()), "{env}");
    assert!(data["env"]["WEB_URL"].as_str().unwrap().contains("unverified.stack.invalid"), "{env}");
    assert!(data["unset"].as_array().unwrap().contains(&json!("WEB_PORT")), "{env}");
    let withheld: Vec<&str> = data["withheld"].as_array().unwrap().iter().map(|w| w["service"].as_str().unwrap()).collect();
    assert_eq!(withheld, ["web", "worker"], "{env}");
    assert!(data["env"].as_object().unwrap().keys().all(|k| !k.starts_with("MISE_")), "{env}");

    // Shell output says so in a comment and evaluates to the same values.
    let out = fixture.ok(&["env", "--shell", "bash"]);
    let text = stdout(&out);
    assert!(text.contains("# stack: web not verified ("), "{text}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not verified: web"));
    let script = format!("{text}\necho \"$WEB_URL|${{WEB_PORT-ABSENT}}|$PLAIN\"");
    let evaluated = Command::new("bash").args(["-c", &script]).env("WEB_PORT", "1").output().unwrap();
    assert_eq!(stdout(&evaluated), format!("{}|ABSENT|it's\n", data["env"]["WEB_URL"].as_str().unwrap()));
    let fish = stdout(&fixture.ok(&["env", "--shell", "fish"]));
    assert!(fish.contains("set -e WEB_PORT") && fish.contains("set -gx PATH (string split : -- "), "{fish}");
    let out = fixture.command_in(&fixture.app(), &["env", "--json", "--shell", "zsh"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "usage");
    assert_eq!(out.status.code(), Some(2));

    let app = fixture.app();
    let replies = fixture.mcp(&app, &[("stack_env", json!({ "dir": app }))]);
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    assert_eq!(replies[0]["data"]["env"]["WEB_URL"], data["env"]["WEB_URL"], "{}", replies[0]);
}

#[test]
fn the_direnv_hook_prints_without_a_project() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let elsewhere = fixture.dir.path().join("bin");
    let hook = stdout(&fixture.ok_in(&elsewhere, &["hook", "direnv"]));
    assert!(hook.contains("eval \"$(stack env --shell bash)\"") && hook.contains("watch_file stack.toml stack.lock .stack/session.json"), "{hook}");
}

#[test]
fn completions_print_without_a_project() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let elsewhere = fixture.dir.path().join("bin");
    for shell in ["bash", "zsh", "fish"] {
        let script = stdout(&fixture.ok_in(&elsewhere, &["completions", shell]));
        assert!(script.contains("stack") && script.contains("logs") && script.contains("completions"), "{shell}: {script}");
    }
}

#[test]
fn the_gc_agent_is_written_printed_and_removed_under_home_without_loading_anything() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    // A HOME of the test's own: nothing is written to, or loaded into, this machine's.
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = fixture.command_in(fixture.dir.path(), args).env("HOME", home.path()).env_remove("XDG_CONFIG_HOME").output().unwrap();
        json_result(&out)
    };
    let expected = if cfg!(target_os = "macos") {
        home.path().join("Library/LaunchAgents/dev.stack.gc.plist")
    } else {
        home.path().join(".config/systemd/user/stack-gc.service")
    };
    let printed = run(&["setup", "--gc-agent", "--print", "--json"]);
    assert_eq!(printed["data"]["written"], false, "{printed}");
    assert!(printed["data"]["content"].as_str().unwrap().contains("--watch"));
    assert!(!expected.exists());

    let written = run(&["setup", "--gc-agent", "--json"]);
    assert_eq!(written["data"]["path"], expected.to_str().unwrap(), "{written}");
    assert_eq!(written["data"]["ran"], false);
    let content = fs::read_to_string(&expected).unwrap();
    assert!(content.contains("gc") && content.contains("--watch"), "{content}");
    // The store it collects is the one the caller uses.
    assert!(content.contains(&fixture.dir.path().join("state").display().to_string()), "{content}");
    let command = written["data"]["command"].as_str().unwrap();
    assert!(command.contains(if cfg!(target_os = "macos") { "launchctl bootstrap" } else { "systemctl --user" }), "{command}");

    let removed = run(&["setup", "--gc-agent", "--remove", "--json"]);
    assert_eq!(removed["data"]["removed"], true, "{removed}");
    assert!(!expected.exists());
    let error = run(&["setup", "--print", "--json"]);
    assert_eq!(error["error"]["code"], "usage", "--print needs --gc-agent: {error}");
}

#[test]
fn mcp_init_writes_a_project_in_dir_and_never_replaces_one() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    fs::create_dir(fixture.dir.path().join("fresh")).unwrap();
    let fresh = fixture.dir.path().join("fresh").canonicalize().unwrap();
    let calls = [
        ("stack_init", json!({ "dir": fresh, "tools": ["python=3.13"], "services": ["redis@8"] })),
        ("stack_init", json!({ "dir": fresh })),
        ("stack_init", json!({ "dir": fixture.app(), "tools": [3] })),
    ];
    let replies = fixture.mcp(&fixture.app(), &calls);
    assert_eq!(replies[0]["data"]["services"], json!(["redis"]), "{}", replies[0]);
    assert_eq!(replies[0]["project"], fresh.to_str().unwrap());
    let written = fs::read_to_string(fresh.join("stack.toml")).unwrap();
    assert!(written.contains("python = \"3.13\""), "{written}");
    assert_eq!(replies[1]["error"]["code"], "project_exists", "{}", replies[1]);
    assert_eq!(replies[2]["error"]["code"], "usage", "{}", replies[2]);
}

#[test]
fn init_detect_adds_what_the_checkout_suggests_on_the_cli_and_over_mcp() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let make = |name: &str| {
        let dir = fixture.dir.path().join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(".python-version"), "3.12\n").unwrap();
        fs::write(dir.join("uv.lock"), "").unwrap();
        fs::write(dir.join("compose.yml"), "services:\n  db:\n    image: postgres:17\n").unwrap();
        dir.canonicalize().unwrap()
    };
    let cli = make("cli");
    let out = fixture.command_in(&cli, &["-C", cli.to_str().unwrap(), "init", "--detect", "--tool", "python=3.13", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["data"]["tools"], json!(["python", "uv"]), "{result}");
    assert_eq!(result["data"]["services"], json!(["postgres"]), "{result}");
    assert_eq!(
        result["data"]["detected"],
        json!([
            { "kind": "tool", "name": "uv", "value": "latest", "source": "uv.lock" },
            { "kind": "service", "name": "postgres", "value": "postgres@17", "source": "compose.yml" }
        ]),
        "{result}"
    );
    let written = fs::read_to_string(cli.join("stack.toml")).unwrap();
    assert!(written.contains("python = \"3.13\"") && written.contains("version = \"17\""), "{written}");

    let mcp = make("mcp");
    let plain = make("plain");
    let replies = fixture.mcp(&fixture.app(), &[("stack_init", json!({ "dir": mcp, "detect": true })), ("stack_init", json!({ "dir": plain }))]);
    assert_eq!(replies[0]["data"]["tools"], json!(["python", "uv"]), "{}", replies[0]);
    assert_eq!(replies[0]["data"]["detected"][0], json!({ "kind": "tool", "name": "python", "value": "3.12", "source": ".python-version" }), "{}", replies[0]);
    // Without detect, the result is what it always was: no `detected` key.
    assert!(replies[1]["data"].get("detected").is_none(), "{}", replies[1]);
    assert_eq!(replies[1]["data"]["tools"], json!([]), "{}", replies[1]);
}
