#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

/// mise as stack drives it: answers from files a test writes under `$REVIEW_FIXTURE`.
const FAKE_MISE: &str = include_str!("fakes/mise.sh");

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
        fs::write(&mise, FAKE_MISE).unwrap();
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
            .env("REVIEW_PYTHON", python::python())
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
fn exec_refuses_a_pin_that_is_not_installed_instead_of_running_another_release_on_path() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.8.2'\n[tasks.q]\nrun='jq --version'\n");
    // Another jq on the stack's PATH, as a system one would be when the pin is missing.
    write_exe(&fixture.dir.path().join("bin/jq"), "#!/bin/sh\ntouch \"$REVIEW_FIXTURE/fallback-ran\"\necho jq-1.7.1\n");
    let ran = fixture.dir.path().join("fallback-ran");
    for cmd in [&["jq", "--version"][..], &["sh", "-c", "jq --version"]] {
        let mut args = vec!["--json", "exec", "--"];
        args.extend_from_slice(cmd);
        let out = fixture.command(&args).output().unwrap();
        let error = &json_result(&out)["error"];
        assert_eq!(error["code"], "tools_not_installed", "{cmd:?}: {error}");
        assert_eq!(error["details"], json!([{ "tool": "jq", "version": "1.8.2" }]), "{cmd:?}");
        assert!(error["hint"].as_str().unwrap().contains("stack install"), "{error}");
        assert!(!ran.exists(), "{cmd:?}: the unpinned jq ran");
    }
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["sh", "-c", "jq --version"] }))], &[]);
    assert_eq!(results[0]["structuredContent"]["error"]["code"], "tools_not_installed", "{}", results[0]);
    assert!(!ran.exists(), "MCP: the unpinned jq ran");
    // A task is still handed to `mise run`, which installs what its configuration names.
    assert_eq!(String::from_utf8_lossy(&fixture.ok(&["run", "q"]).stdout), "q|");

    fixture.ok(&["install"]);
    let out = fixture.ok(&["--json", "exec", "--", "sh", "-c", "jq --version"]);
    assert_eq!(json_result(&out)["data"]["exit_code"], 0);
    // What mise was asked: the pin as stack.lock records it, from a scratch root.
    let ls = fs::read_to_string(fixture.dir.path().join("ls.log")).unwrap();
    assert!(ls.contains("jq = \"1.8.2\"") && !ls.contains(&format!("dir={}", fixture.dir.path().join("app").canonicalize().unwrap().display())), "{ls}");
}

/// `git <args>` in `dir`, which must succeed; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// Untracked paths git reports under `dir`, ignored ones left out.
fn untracked(dir: &Path, under: &str) -> Vec<String> {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .lines()
        .filter_map(|l| l.strip_prefix("?? "))
        .filter(|p| p.starts_with(under))
        .map(String::from)
        .collect()
}

#[test]
fn generated_files_are_ignored_by_each_checkouts_own_gitignore_and_user_files_are_not() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    let top = fixture.dir.path();
    git(top, &["init", "-q"]);
    let exclude = top.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    // The user's lines, and a block an earlier release kept for another checkout, which hid a
    // file of the user's in this one.
    let mine = "# mine\n*.log\n";
    fs::write(&exclude, format!("{mine}\n# stack: generated files of /elsewhere/app\n/app/.claude/skills/mine/\n/app/.config/mise/config.toml\n# stack: end of /elsewhere/app\n")).unwrap();
    // Files of the user's own beside stack's: a mise config and a hand-written skill.
    fs::write(top.join("app/.config/mise/config.toml"), "[tools]\n").unwrap();
    fs::create_dir_all(top.join("app/.claude/skills/mine")).unwrap();
    fs::write(top.join("app/.claude/skills/mine/SKILL.md"), "mine\n").unwrap();
    fs::write(top.join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[skills]\ndir='.claude/skills'\n").unwrap();
    fixture.ok(&["up"]);
    // What mise writes beside them when a lock is rendered (this fake locks nothing).
    fs::write(top.join("app/.config/mise/mise.lock"), "").unwrap();
    fs::create_dir_all(top.join("app/.config/mise/locks")).unwrap();
    fs::write(top.join("app/.config/mise/locks/x"), "").unwrap();
    assert!(top.join("app/.config/mise/conf.d/stack.toml").exists() && top.join("app/.stack").exists());
    assert_eq!(
        untracked(top, "app/"),
        ["app/.claude/skills/mine/SKILL.md", "app/.config/mise/config.toml", "app/stack.lock", "app/stack.toml"],
    );
    // The shared exclude holds the user's lines only: the old block, and what it hid, are gone.
    assert_eq!(fs::read_to_string(&exclude).unwrap(), mine);
    assert!(!top.join(".git/info/exclude.lock").exists());
    let ignore = fs::read_to_string(top.join("app/.config/mise/.gitignore")).unwrap();
    assert!(ignore.starts_with("# Generated by stack") && ignore.contains("\n/.gitignore\n/conf.d/stack.toml\n/mise.lock\n/locks/\n"), "{ignore}");

    // A worktree of the same repository: its own ignore files; the shared exclude is untouched.
    git(top, &["add", "bundle", "app/stack.toml", "app/stack.lock"]);
    git(top, &["commit", "-qm", "stack"]);
    let wt = top.join("wt");
    git(top, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let out = fixture.command_at(&wt.join("app"), &["compile"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(wt.join("app/.config/mise/conf.d/stack.toml").exists() && wt.join("app/.config/mise/.gitignore").exists());
    assert_eq!(untracked(&wt, "app/"), Vec::<String>::new());
    assert_eq!(fs::read_to_string(&exclude).unwrap(), mine);

    // A .gitignore of the user's is never changed: stack warns about what it leaves unignored.
    fs::write(top.join("app/.config/mise/.gitignore"), "# mine\n/conf.d/stack.toml\n").unwrap();
    let report = json_result(&fixture.ok(&["--json", "compile"]));
    let warnings = report["data"]["warnings"].to_string();
    // (compile removed the empty mise.lock: this fake locks nothing.)
    assert!(warnings.contains("is not stack's") && warnings.contains("generated locks") && !warnings.contains("conf.d/stack.toml"), "{warnings}");
    assert_eq!(fs::read_to_string(top.join("app/.config/mise/.gitignore")).unwrap(), "# mine\n/conf.d/stack.toml\n");

    // Outside a repository nothing is written anywhere.
    fs::remove_dir_all(top.join(".git")).unwrap();
    fs::remove_file(top.join("app/.config/mise/.gitignore")).unwrap();
    fixture.ok(&["compile"]);
    assert!(!top.join(".git").exists() && !top.join("app/.config/mise/.gitignore").exists());
}

#[test]
fn concurrent_compiles_of_many_projects_in_one_repository_each_keep_their_files_ignored() {
    let fixture = Fixture::new();
    let top = fixture.dir.path();
    git(top, &["init", "-q"]);
    let exclude = top.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    // Old per-checkout blocks, removed once whichever compile gets git's lock first.
    let mine = "# mine\n*.log\n";
    let mut text = mine.to_string();
    for i in 0..32 {
        text.push_str(&format!("\n# stack: generated files of {top}/p{i}\n/p{i}/.stack/\n# stack: end of {top}/p{i}\n", top = top.display()));
    }
    fs::write(&exclude, text).unwrap();
    let projects: Vec<std::path::PathBuf> = (0..32).map(|i| top.join(format!("p{i}"))).collect();
    for p in &projects {
        fs::create_dir_all(p).unwrap();
        fs::write(p.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
    }
    let outs: Vec<Output> = thread::scope(|scope| {
        let handles: Vec<_> = projects.iter().map(|p| scope.spawn(|| fixture.command_at(p, &["--json", "compile"]).output().unwrap())).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for out in &outs {
        let result = json_result(out);
        assert_eq!(result["ok"], true, "{result}");
        assert!(result["data"].get("warnings").map_or(true, |w| !w.to_string().contains("exclude")), "{result}");
    }
    assert_eq!(fs::read_to_string(&exclude).unwrap(), mine);
    assert!(!top.join(".git/info/exclude.lock").exists());
    for (i, p) in projects.iter().enumerate() {
        assert!(p.join(".config/mise/.gitignore").exists(), "p{i}");
        let left = untracked(top, &format!("p{i}/"));
        assert!(left.iter().all(|f| f.ends_with("stack.toml") && !f.contains(".config") || f.ends_with("stack.lock")), "p{i}: {left:?}");
    }
}

#[test]
fn a_contended_old_exclude_migration_ends_with_the_calls_deadline_and_changes_nothing() {
    let fixture = Fixture::new();
    fixture.ok(&["compile"]);
    let top = fixture.dir.path();
    git(top, &["init", "-q"]);
    let exclude = top.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    let old = "# mine\n\n# stack: generated files of /elsewhere/app\n/app/.stack/\n# stack: end of /elsewhere/app\n";
    fs::write(&exclude, old).unwrap();
    // Another writer holds git's lock on the file, for longer than any call here allows.
    let lock = top.join(".git/info/exclude.lock");
    fs::write(&lock, "another writer's\n").unwrap();
    let marker = top.join("ran");
    let cmd = format!("touch '{}'", marker.display());
    let check = |route: &str, error: &Value, elapsed: Duration| {
        assert_eq!(error["code"], "timed_out", "{route}: {error}");
        assert!(elapsed < Duration::from_secs(3), "{route}: the 5s migration wait outlived the 1s call: {elapsed:?}");
        assert!(!marker.exists(), "{route}: the command ran");
        assert_eq!(fs::read_to_string(&exclude).unwrap(), old, "{route}");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "another writer's\n", "{route}");
    };
    let started = Instant::now();
    let out = fixture.command(&["--json", "exec", "--timeout", "1s", "--", "sh", "-c", &cmd]).output().unwrap();
    check("cli", &json_result(&out)["error"], started.elapsed());
    let started = Instant::now();
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["sh", "-c", cmd], "timeout_secs": 1 }))], &[]);
    check("mcp", &results[0]["structuredContent"]["error"], started.elapsed());
    // Without a deadline the migration still waits its own while for the lock, then removes the block.
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        fs::remove_file(&lock).unwrap();
    });
    fixture.ok(&["compile"]);
    release.join().unwrap();
    assert_eq!(fs::read_to_string(&exclude).unwrap(), "# mine\n");
}

#[test]
fn install_prints_only_the_kinds_of_install_that_ran() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    let text = String::from_utf8(fixture.ok(&["install"]).stdout).unwrap();
    assert!(text.contains("unchecked (mise install): jq\n") && !text.contains("checked (mise install --locked)"), "{text}");
    let text = String::from_utf8(Fixture::new().ok(&["install"]).stdout).unwrap();
    assert!(!text.contains("mise install"), "nothing was installed: {text}");
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
fn a_timed_out_command_releases_its_execution_even_while_the_project_is_locked() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    // Another command holds the project lock when the timed-out command ends and its
    // execution is released, after the run's deadline has passed.
    let holder = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fs::read_to_string(&session_file).is_ok_and(|s| s.contains("active_executions\": {\n    \"")) {
            assert!(Instant::now() < deadline, "execution never registered");
            thread::sleep(Duration::from_millis(10));
        }
        let _lock = stack::state::project_lock(&state, &app).unwrap();
        thread::sleep(Duration::from_millis(2500));
    });
    // The same server then starts the stack again: no execution of its own is left behind.
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["sleep", "5"], "timeout_secs": 1 })), ("stack_up", json!({}))], &[]);
    holder.join().unwrap();
    assert_eq!(results[0]["structuredContent"]["error"]["code"], "timed_out", "{}", results[0]);
    assert_eq!(results[1]["structuredContent"]["ok"], true, "{}", results[1]);
}

/// One interactive `stack mcp` server: each call is answered before the next is sent.
struct Server {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: std::io::BufReader<std::process::ChildStdout>,
    app: std::path::PathBuf,
    id: u32,
}

impl Server {
    fn start(fixture: &Fixture) -> Self {
        let mut child = fixture.command(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        Server { child, input, output, app: fixture.dir.path().join("app"), id: 0 }
    }

    fn call(&mut self, name: &str, mut args: Value) -> Value {
        use std::io::{BufRead, Write};
        self.id += 1;
        args["dir"] = json!(self.app);
        writeln!(self.input, "{}", json!({"jsonrpc":"2.0","id":self.id,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["result"]["structuredContent"].clone()
    }

    fn close(self) {
        let Server { mut child, input, .. } = self;
        drop(input);
        assert!(child.wait().unwrap().success());
    }
}

fn read_value(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Wait until `session_file` records an execution, then hold the project lock for `hold`.
fn hold_project_lock_once_executing(state: &Path, app: &Path, session_file: &Path, hold: Duration) -> thread::JoinHandle<()> {
    let (state, app, session_file) = (state.to_path_buf(), app.to_path_buf(), session_file.to_path_buf());
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while read_value(&session_file)["active_executions"].as_object().map_or(true, |e| e.is_empty()) {
            assert!(Instant::now() < deadline, "execution never registered");
            thread::sleep(Duration::from_millis(10));
        }
        let _lock = stack::state::project_lock(&state, &app).unwrap();
        thread::sleep(hold);
    })
}

/// This fixture's completion records: `state/completed/<project key>/`.
fn completions(fixture: &Fixture) -> std::path::PathBuf {
    let index = fixture.session_paths().pop().unwrap();
    fixture.dir.path().join("state/completed").join(index.file_stem().unwrap())
}

#[test]
fn a_timed_out_command_whose_release_outwaits_its_grace_still_frees_the_session() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    let records = completions(&fixture);
    // Another command holds the project lock for longer than the release waits for it.
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_secs(12));
    let mut server = Server::start(&fixture);
    let started = Instant::now();
    let result = server.call("stack_exec", json!({ "command": ["sleep", "30"], "timeout_secs": 1 }));
    let elapsed = started.elapsed();
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    assert!(elapsed < Duration::from_secs(12), "the call stays bounded: {elapsed:?}");
    // While the lock is still held: the execution is recorded, with a record that it finished.
    let recorded = read_value(&session_file)["active_executions"].clone();
    let (token, pid) = recorded.as_object().unwrap().iter().next().unwrap_or_else(|| panic!("{recorded}"));
    let completion = read_value(&records.join(format!("{token}.json")));
    assert_eq!(completion["token"], json!(token));
    assert_eq!(completion["pid"], *pid);
    assert_eq!(completion["pid"], server.child.id());
    assert_eq!(completion["session"], read_value(&session_file)["id"]);
    holder.join().unwrap();
    // Neither another process, while the server lives, nor the same server sees it as busy.
    let out = fixture.command(&["--json", "up"]).output().unwrap();
    assert_eq!(json_result(&out)["ok"], true, "another process: {}", String::from_utf8_lossy(&out.stdout));
    let result = server.call("stack_up", json!({}));
    assert_eq!(result["ok"], true, "the same server: {result}");
    assert_eq!(read_value(&session_file)["active_executions"], json!({}));
    assert_eq!(fs::read_dir(&records).unwrap().count(), 0, "the applied record is removed");
    server.close();
    fixture.ok(&["down"]);
    assert!(!records.exists(), "down removes the project's completion records");
}

/// Record `token` as executing for this (live) test process, in both session files.
fn record_execution(fixture: &Fixture, token: &str, renewed_at: Option<u64>) -> Value {
    let mut session = read_value(&fixture.session_paths().pop().unwrap());
    session["active_executions"][token] = json!(std::process::id());
    if let Some(at) = renewed_at {
        session["lease"]["renewed_at"] = json!(at);
    }
    for path in fixture.session_paths() {
        if path.parent().unwrap().exists() {
            fs::write(&path, session.to_string()).unwrap();
        }
    }
    session
}

/// The completion record that execution would have left.
fn complete(fixture: &Fixture, session: &Value, token: &str, completed_at: u64) -> std::path::PathBuf {
    let dir = completions(fixture);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{token}.json"));
    let record = json!({ "session": session["id"], "token": token, "pid": std::process::id(), "completed_at": completed_at });
    fs::write(&path, record.to_string()).unwrap();
    path
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[test]
fn a_finished_execution_is_settled_and_a_running_one_still_keeps_the_session_busy() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let session_file = fixture.dir.path().join("app/.stack/session.json");
    // Another process's command, still running.
    let mut running = fixture.command(&["exec", "--", "sleep", "4"]).stdout(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_value(&session_file)["active_executions"].as_object().map_or(true, |e| e.is_empty()) {
        assert!(Instant::now() < deadline, "execution never registered");
        thread::sleep(Duration::from_millis(10));
    }
    let other = read_value(&session_file)["active_executions"].as_object().unwrap().keys().next().unwrap().clone();
    // And one that finished but could not remove itself.
    let done = "d".repeat(64);
    let session = record_execution(&fixture, &done, None);
    let record = complete(&fixture, &session, &done, unix_now());
    let out = fixture.command(&["--json", "up"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "session_busy", "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(read_value(&session_file)["active_executions"], json!({ other.clone(): running.id() }), "only the finished one is forgotten");
    assert!(!record.exists());
    assert!(running.wait().unwrap().success());
    fixture.ok(&["up"]);
}

#[test]
fn gc_applies_a_completion_renewing_the_lease_and_reclaims_a_deleted_project_it_unblocks() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "30s"]);
    let index = fixture.session_paths().pop().unwrap();
    let token = "e".repeat(64);
    // Idle long past the TTL by the record, but its command finished just now.
    let session = record_execution(&fixture, &token, Some(unix_now() - 120));
    let finished = unix_now();
    let record = complete(&fixture, &session, &token, finished);
    let out = fixture.ok(&["gc", "--json"]);
    assert_eq!(json_result(&out)["data"], json!([]), "the lease counts from when the command finished");
    let saved = read_value(&index);
    assert_eq!(saved["active_executions"], json!({}));
    assert!(saved["lease"]["renewed_at"].as_u64().unwrap() >= finished, "{saved}");
    assert!(!record.exists());

    // A deleted project whose only execution finished is reclaimed, not kept as busy, and
    // nothing recreates the directory.
    let session = record_execution(&fixture, &token, None);
    let record = complete(&fixture, &session, &token, unix_now());
    let app = fixture.dir.path().join("app");
    fs::remove_dir_all(&app).unwrap();
    let (ok, result) = fixture.gc(&[]);
    assert!(ok, "{result}");
    let reclaimed = result["data"].clone();
    assert_eq!(reclaimed[0]["reason"], "project directory deleted", "{reclaimed}");
    assert_eq!(reclaimed[0]["stopped"], true, "{reclaimed}");
    assert!(!index.exists() && !record.parent().unwrap().exists());
    assert!(!app.exists(), "gc recreated the deleted project");
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
    fixture.ok(&["install"]);
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
    fixture.ok(&["install"]);
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
    fixture.ok(&["install"]);
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
fn mcp_rejects_malformed_arguments_before_any_work() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.t]\nrun='true'\n");
    let root = fixture.dir.path();
    let _ = fs::remove_file(root.join("mise.log"));
    let ran = ["sh", "-c", "touch \"$REVIEW_FIXTURE/ran\""];
    // (tool, arguments, what the message names, what the hint says)
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        ("stack_exec", json!({ "command": ran, "secret": ["DEPLOY_KEY"] }), "unknown argument `secret`", "did you mean `secrets`?"),
        ("stack_exec", json!({ "command": ran, "timeout": "1s" }), "unknown argument `timeout`", "did you mean `timeout_secs`?"),
        ("stack_exec", json!({ "command": "echo hi" }), "`command` must be an array", r#"["sh", "-c", "echo hi"]"#),
        ("stack_exec", json!({ "command": ["echo", 1] }), "`command` item 1: must be a string", ""),
        ("stack_exec", json!({ "command": [] }), "`command` must not be empty", ""),
        ("stack_exec", json!({}), "`command` is required", ""),
        ("stack_exec", json!({ "command": null }), "`command` is required", ""),
        ("stack_exec", json!({ "command": ran, "timeout_secs": "1s" }), "`timeout_secs` must be a whole number", ""),
        ("stack_exec", json!({ "command": ran, "timeout_secs": 0 }), "`timeout_secs` must be at least 1", ""),
        ("stack_exec", json!({ "command": ran, "timeout_secs": 1.5 }), "`timeout_secs` must be a whole number", ""),
        ("stack_exec", json!({ "command": ran, "require": "db" }), "`require` must be an array", ""),
        ("stack_exec", json!({ "command": ran, "require": [null] }), "`require` item null", ""),
        ("stack_exec", json!({ "command": ran, "require": ["db"], "require_all": true }), "`require` and `require_all` cannot be given together", ""),
        ("stack_exec", json!({ "command": ran, "require_all": "yes" }), "`require_all` must be true or false", ""),
        ("stack_exec", json!({ "command": ran, "secrets": "DEPLOY_KEY" }), "`secrets` must be an array", ""),
        ("stack_exec", json!({ "command": ran, "secrets": ["deploy_key"] }), "is not a secret name", ""),
        ("stack_run", json!({ "task": "t", "secrets": ["A"] }), "unknown argument `secrets`", "secrets the task declares"),
        ("stack_run", json!({ "task": "t", "args": "a b" }), "`args` must be an array", ""),
        ("stack_run", json!({ "task": "t", "args": ["a", false] }), "`args` item false", ""),
        ("stack_run", json!({ "task": 5 }), "`task` must be a string", ""),
        ("stack_run", json!({ "task": "t", "timeout": 5 }), "unknown argument `timeout`", "did you mean `timeout_secs`?"),
        ("stack_compile", json!({ "update": true, "locked": true }), "`update` and `locked` cannot be given together", ""),
        ("stack_compile", json!({ "update": "yes" }), "`update` must be true or false", ""),
        ("stack_up", json!({ "ttl": 30 }), "`ttl` must be a string", ""),
        ("stack_up", json!({ "owner": 1 }), "unknown argument `owner`", "did you mean `owner_pid`?"),
        ("stack_restart", json!({ "services": "web" }), "`services` must be an array", ""),
        ("stack_logs", json!({ "service": "web", "tail": 0 }), "`tail` must be from 1 to 10000", ""),
        ("stack_logs", json!({ "tail": 5 }), "`service` is required", ""),
        ("stack_inspect", json!({ "all_skills": 1 }), "`all_skills` must be true or false", ""),
        ("stack_skill", json!({ "tool": "jq", "name": 3 }), "`name` must be a string", ""),
        ("stack_status", json!({ "verbose": true }), "unknown argument `verbose`", "stack_status accepts: dir"),
        ("stack_gc", json!({ "force": true }), "unknown argument `force`", ""),
        ("stack_down", json!({ "dir_": "x" }), "unknown argument `dir_`", "did you mean `dir`?"),
    ];
    let calls: Vec<(&str, Value)> = cases.iter().map(|(tool, args, _, _)| (*tool, args.clone())).collect();
    for (result, (tool, args, message, hint)) in fixture.mcp(&calls, &[]).iter().zip(&cases) {
        let error = &result["structuredContent"]["error"];
        assert_eq!((result["isError"].as_bool(), error["code"].as_str()), (Some(true), Some("usage")), "{tool} {args}: {result}");
        assert!(error["message"].as_str().unwrap().contains(message), "{tool} {args}: {error}");
        assert!(error["hint"].as_str().unwrap_or_default().contains(hint), "{tool} {args}: {error}");
    }
    assert!(!root.join("ran").exists(), "a command ran");
    assert!(!root.join("mise.log").exists(), "provider work ran: {}", fs::read_to_string(root.join("mise.log")).unwrap_or_default());
    assert!(!root.join("app/.stack/session.json").exists() && !root.join("state/sessions").exists());

    // `null` still means omitted, and well-formed calls run.
    let results = fixture.mcp(
        &[
            ("stack_exec", json!({ "command": ran, "secrets": null, "require": null, "require_all": null, "timeout_secs": null })),
            ("stack_exec", json!({ "command": ["true"], "require": [], "require_all": true, "timeout_secs": u64::MAX })),
            ("stack_run", json!({ "task": "t", "args": null })),
        ],
        &[],
    );
    for result in &results {
        assert_eq!(result["structuredContent"]["data"]["exit_code"], 0, "{result}");
    }
    assert!(root.join("ran").exists());
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
fn every_gc_mode_shows_the_recovery_commands() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    fixture.supervisor_tracks(running.service.0, running.port);
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();

    let (ok, result) = fixture.gc(&[]);
    assert!(!ok, "{result}");
    let recovery = &result["error"]["details"][0]["services"][0]["recovery"];
    let commands = [recovery["inspect"].as_str().unwrap(), recovery["stop"].as_str().unwrap()];
    for args in [&["gc"][..], &["gc", "--watch", "--max-passes", "1"], &["gc", "--watch", "--max-passes", "1", "--json"]] {
        let out = fixture.command_at(fixture.dir.path(), args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        // People read stderr; with --json, each pass is one object on stdout.
        let shown = match args.contains(&"--json") {
            true => serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["hint"].as_str().unwrap().to_string(),
            false => String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        for command in commands {
            assert!(shown.contains(command), "{args:?} omits {command}:\n{shown}");
        }
    }
    assert!(running.service.alive(), "GC must not stop by daemon name");
    assert!(!fixture.pitchfork_log().contains(" stop "));
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

#[test]
fn a_supervisor_stack_starts_runs_daemons_with_the_mise_stack_runs() {
    // Pitchfork finds mise for `mise x` only in a few fixed places, unless told. A supervisor
    // that cannot find it runs every project's daemons with the tools of the project that
    // started it, so another project's service gets another release.
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up", "--json"]);
    let log = fs::read_to_string(fixture.dir.path().join("supervisor-start.log")).unwrap();
    let mise = fixture.dir.path().join("bin/mise");
    let expected = format!("x -- pitchfork supervisor start|{}", mise.display());
    assert!(!log.is_empty() && log.lines().all(|line| line == expected), "expected `{expected}`:\n{log}");
    fixture.ok(&["down", "--json"]);
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_when_path_names_mise_absolutely() {
    supervisor_outlives_a_timed_out_start(None, "bin/mise", None);
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_when_path_names_mise_relatively() {
    supervisor_outlives_a_timed_out_start(Some("bin"), "app/bin/mise", None);
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_when_mise_is_in_the_project() {
    // An empty PATH entry means the working directory.
    supervisor_outlives_a_timed_out_start(Some(""), "app/mise", None);
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_past_a_directory_named_mise() {
    // The OS skips a directory named `mise` on PATH, searchable or not, for the file after it.
    supervisor_outlives_a_timed_out_start(None, "bin/mise", Some(Decoy::Directory));
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_past_a_relative_directory_named_mise() {
    supervisor_outlives_a_timed_out_start(Some("bin"), "app/bin/mise", Some(Decoy::Directory));
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_past_a_mise_only_others_may_run() {
    // The OS skips a file this user may not run for the file after it, whatever others may.
    supervisor_outlives_a_timed_out_start(None, "bin/mise", Some(Decoy::OthersOnly));
}

#[test]
fn a_supervisor_stack_starts_outlives_a_timed_out_request_past_a_relative_mise_only_others_may_run() {
    supervisor_outlives_a_timed_out_start(Some("bin"), "app/bin/mise", Some(Decoy::OthersOnly));
}

/// What comes before the mise to find on PATH, named `mise` in an entry of the same kind.
enum Decoy {
    /// A searchable directory.
    Directory,
    /// A file, mode 0645: executable by others, but not by its owner, the user running stack.
    OthersOnly,
}

/// The wrong mise: one stack must never run. It records that it ran.
const WRONG_MISE: &str = "#!/bin/sh\necho \"$0 $*\" >>\"$REVIEW_FIXTURE/wrong-mise.log\"\nexit 1\n";

fn running_as_root() -> bool {
    // SAFETY: geteuid cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// `stack -C app up --timeout 2s` from another directory, with mise on PATH as `entry` names it
/// (`None`: the fixture's absolute `bin`) and found at `mise_at`. The directory stack runs in
/// has its own `bin/mise` and `mise`, which a relative entry must not find: the provider runs
/// mise in the project. The request is cut short; the supervisor stack started before it is
/// in a session of its own, so it survives, and the request never starts one in its group.
/// A `decoy` is in an entry of the same kind before it (`decoy/mise`, under the project when
/// relative).
fn supervisor_outlives_a_timed_out_start(entry: Option<&str>, mise_at: &str, decoy: Option<Decoy>) {
    if matches!(decoy, Some(Decoy::OthersOnly)) && running_as_root() {
        // Root may run a file any execute bit allows, and the OS runs the decoy for it too.
        eprintln!("skipped: needs a user other than root");
        return;
    }
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let mise = dir.join(mise_at);
    if !mise.exists() {
        fs::create_dir_all(mise.parent().unwrap()).unwrap();
        fs::copy(dir.join("bin/mise"), &mise).unwrap();
    }
    let caller = dir.join("caller");
    fs::create_dir_all(caller.join("bin")).unwrap();
    for name in ["bin/mise", "mise"] {
        fs::write(caller.join(name), WRONG_MISE).unwrap();
        fs::set_permissions(caller.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The rest of PATH, without any other mise.
    let rest = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|d| d.is_absolute() && !d.join("mise").exists())
        .collect::<Vec<_>>();
    let first = entry.map_or_else(|| dir.join("bin"), std::path::PathBuf::from);
    let decoy = decoy.map(|kind| {
        let entry = if entry.is_some() { std::path::PathBuf::from("decoy") } else { dir.join("decoy") };
        // An absolute entry replaces the project path it is joined to.
        let decoy = dir.join("app").join(&entry).join("mise");
        match kind {
            Decoy::Directory => {
                fs::create_dir_all(&decoy).unwrap();
                fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).unwrap();
            }
            Decoy::OthersOnly => {
                fs::create_dir_all(decoy.parent().unwrap()).unwrap();
                fs::write(&decoy, WRONG_MISE).unwrap();
                fs::set_permissions(&decoy, fs::Permissions::from_mode(0o645)).unwrap();
            }
        }
        entry
    });
    let path = std::env::join_paths(decoy.into_iter().chain([first]).chain(rest)).unwrap();
    let run = |args: &[&str]| fixture.command(args).current_dir(&caller).env("PATH", &path).output().unwrap();
    fs::write(dir.join("supervisor-process"), "").unwrap();
    fs::write(dir.join("slow-start"), "30").unwrap();
    fs::remove_file(dir.join("mise-self.log")).unwrap();

    let out = run(&["up", "--timeout", "2s", "--json"]);
    assert_eq!(out.status.code(), Some(124), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(json_result(&out)["error"]["details"][0]["cause"]["code"], "start_failed");
    let recorded = fs::read_to_string(dir.join("supervisor-pid")).expect("no supervisor started");
    let (pid, by) = recorded.trim().split_once(' ').unwrap();
    let supervisor = Detached(pid.parse().unwrap());
    assert_eq!(by, "detached", "the request started the supervisor in the group its deadline kills");
    assert!(supervisor.alive(), "the supervisor ended with the timed-out request");
    // The supervisor was started with, and told to run daemons with, the mise the request ran.
    let expected = mise.canonicalize().unwrap();
    let starts = fs::read_to_string(dir.join("supervisor-start.log")).unwrap();
    let told = starts.trim().strip_prefix("x -- pitchfork supervisor start|").unwrap();
    assert_eq!(Path::new(told).canonicalize().unwrap(), expected, "{starts}");
    let ran = fs::read_to_string(dir.join("mise-self.log")).unwrap();
    for call in ["x --", "daemons start"] {
        assert!(ran.lines().any(|l| l.ends_with(call)), "no `mise {call}`:\n{ran}");
    }
    assert!(ran.lines().all(|l| Path::new(l.rsplitn(3, ' ').last().unwrap()) == expected), "{ran}");
    assert!(!dir.join("wrong-mise.log").exists(), "{}", fs::read_to_string(dir.join("wrong-mise.log")).unwrap());

    // Later commands see the launch still recorded, stop it, and leave the supervisor running.
    fs::remove_file(dir.join("slow-start")).unwrap();
    let status = json_result(&run(&["status", "--json"]));
    assert_eq!(status["data"]["session"]["launching"], true, "{status}");
    let down = run(&["down", "--json"]);
    assert_eq!(json_result(&down)["data"]["confirmed"], true, "{}", String::from_utf8_lossy(&down.stdout));
    assert!(supervisor.alive());
    assert_eq!(fs::read_to_string(dir.join("supervisor-pid")).unwrap(), recorded);
    assert!(!dir.join("wrong-mise.log").exists());
}

/// The `mise` calls the fixture has seen since `from` lines of its log.
fn mise_calls(fixture: &Fixture, from: usize) -> Vec<String> {
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap_or_default();
    log.lines().skip(from).map(str::to_string).collect()
}

fn no_supervisor_request(calls: &[String]) {
    let requests: Vec<_> = calls.iter().filter(|c| c.starts_with("daemons start") || c.starts_with("daemons stop")).collect();
    assert!(requests.is_empty(), "a request was made, whose client could start the supervisor in a killable group: {requests:?}");
}

#[test]
fn a_supervisor_stack_cannot_start_stops_up_before_any_request_and_a_retry_succeeds() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fs::write(dir.join("supervisor-process"), "").unwrap();
    fs::write(dir.join("x-fail"), "").unwrap();
    let from = mise_calls(&fixture, 0).len();

    let out = fixture.command(&["up", "--timeout", "30s", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert!(!out.status.success() && out.status.code() != Some(124), "{result}");
    let error = &result["error"];
    assert_eq!(error["code"], "start_failed", "{result}");
    assert!(error["message"].as_str().unwrap().contains("`mise x -- pitchfork supervisor start` failed"), "{result}");
    assert!(error["hint"].as_str().unwrap().contains("in the project to see why"), "{result}");
    let calls = mise_calls(&fixture, from);
    assert!(calls.iter().any(|c| c.starts_with("x -- pitchfork supervisor start")), "{calls:?}");
    no_supervisor_request(&calls);
    assert!(!dir.join("supervisor-pid").exists(), "a supervisor was started");

    // Without a deadline `down` is unchanged, and once the supervisor can start, `up` works.
    assert_eq!(json_result(&fixture.ok(&["down", "--json"]))["data"]["confirmed"], true);
    fs::remove_file(dir.join("x-fail")).unwrap();
    fixture.ok(&["up", "--timeout", "30s", "--json"]);
    let recorded = fs::read_to_string(dir.join("supervisor-pid")).expect("no supervisor started");
    let (pid, by) = recorded.trim().split_once(' ').unwrap();
    let supervisor = Detached(pid.parse().unwrap());
    assert_eq!(by, "detached");
    assert!(supervisor.alive());
    fixture.ok(&["down", "--json"]);
}

#[test]
fn a_supervisor_stack_cannot_start_stops_restart_before_it_stops_anything() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up", "--json"]);
    let service = fs::read_to_string(dir.join("pf-tracked-pid")).unwrap().trim().parse::<u32>().unwrap();
    fs::write(dir.join("x-fail"), "").unwrap();
    let from = mise_calls(&fixture, 0).len();

    for args in [&["restart", "--timeout", "30s", "--json"][..], &["restart", "web", "--timeout", "30s", "--json"]] {
        let out = fixture.command(args).output().unwrap();
        let result = json_result(&out);
        assert!(!out.status.success() && out.status.code() != Some(124), "{result}");
        assert_eq!(result["error"]["code"], "stop_failed", "{result}");
        assert!(pid_alive(service), "the service was stopped");
    }
    no_supervisor_request(&mise_calls(&fixture, from));
    fs::remove_file(dir.join("x-fail")).unwrap();
    fixture.ok(&["restart", "web", "--json"]);
    assert!(!pid_alive(service), "the old service survived the restart");
    fixture.ok(&["down", "--json"]);
}

#[test]
fn a_supervisor_stack_cannot_run_stops_up_before_any_request() {
    // A mise first on PATH that names a missing interpreter: the OS skips it for a bare
    // `mise`, but run by path it cannot be started at all.
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    fs::create_dir(dir.join("broken")).unwrap();
    fs::write(dir.join("broken/mise"), "#!/nonexistent/interpreter\n").unwrap();
    fs::set_permissions(dir.join("broken/mise"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(dir.join("supervisor-process"), "").unwrap();
    let path = format!("{}:{}:{}", dir.join("broken").display(), dir.join("bin").display(), std::env::var("PATH").unwrap());
    let from = mise_calls(&fixture, 0).len();

    let out = fixture.command(&["up", "--timeout", "30s", "--json"]).env("PATH", path).output().unwrap();
    let result = json_result(&out);
    assert!(!out.status.success() && out.status.code() != Some(124), "{result}");
    assert_eq!(result["error"]["code"], "provider_unavailable", "{result}");
    assert!(result["error"]["message"].as_str().unwrap().contains("broken/mise"), "{result}");
    no_supervisor_request(&mise_calls(&fixture, from));
    assert!(!dir.join("supervisor-pid").exists(), "a supervisor was started");
}

#[test]
fn a_supervisor_start_still_running_at_the_deadline_is_left_to_finish_and_no_request_is_made() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    fs::write(dir.join("supervisor-process"), "").unwrap();
    fs::write(dir.join("x-hang"), "30").unwrap();
    let from = mise_calls(&fixture, 0).len();

    let started = Instant::now();
    let out = fixture.command(&["up", "--timeout", "2s", "--json"]).output().unwrap();
    let elapsed = started.elapsed();
    let result = json_result(&out);
    let hung = Detached(fs::read_to_string(dir.join("x-hang-pid")).expect("the start never ran").trim().parse().unwrap());
    assert_eq!(out.status.code(), Some(124), "{result}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    let cause = &result["error"]["details"][0]["cause"];
    assert_eq!(cause["code"], "start_failed", "{result}");
    assert!(cause["message"].as_str().unwrap().contains("cut short by the deadline"), "{result}");
    // Left running in a session of its own, not in a group stack kills.
    assert!(hung.alive(), "the start was killed");
    let group = Command::new("ps").args(["-o", "pgid=", "-p", &hung.0.to_string()]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&group.stdout).trim(), hung.0.to_string());
    no_supervisor_request(&mise_calls(&fixture, from));
    assert!(!dir.join("supervisor-pid").exists(), "a supervisor was started");
    // The launch stays recorded for `down`, which needs no deadline.
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["session"]["launching"], true, "{status}");
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
fn tasks_get_the_real_session_or_none_and_a_declared_stack_session_is_refused_first() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.showenv]\nrun='env'\n");
    let show = "printf '%s' \"${STACK_SESSION-unset}\"";
    let session_of = |out: &Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("STACK_SESSION=").map(str::to_string))
    };

    // No session: neither exec nor a task receives one, and the task still runs.
    assert_eq!(String::from_utf8_lossy(&fixture.ok(&["exec", "--", "sh", "-c", show]).stdout), "unset");
    assert_eq!(session_of(&fixture.ok(&["run", "showenv"])), None);

    // A session: both receive its id.
    let up = json_result(&fixture.ok(&["--json", "up"]));
    let id = up["data"]["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(String::from_utf8_lossy(&fixture.ok(&["exec", "--", "sh", "-c", show]).stdout), id);
    assert_eq!(session_of(&fixture.ok(&["run", "showenv"])).as_deref(), Some(id.as_str()));
    fixture.ok(&["down"]);

    // Declaring it is refused before mise is asked for anything.
    fs::write(
        fixture.dir.path().join("app/stack.toml"),
        "[[use]]\nbundle='path:../bundle'\n[env]\nSTACK_SESSION='fixture'\n",
    )
    .unwrap();
    let log = fixture.dir.path().join("mise.log");
    let calls = fs::read_to_string(&log).unwrap();
    for args in [&["--json", "run", "showenv"][..], &["--json", "exec", "--", "true"], &["--json", "compile"]] {
        let out = fixture.command(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        let error = &json_result(&out)["error"];
        assert_eq!(error["code"], "invalid_env", "{args:?}: {error}");
        assert!(error["message"].as_str().unwrap().contains("env.STACK_SESSION (project)"), "{error}");
    }
    assert_eq!(fs::read_to_string(&log).unwrap(), calls, "mise is never run");
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

/// The `timed_out` error of a startup whose deadline passed while verifying `service`, which
/// started but never became ready. Each look at it first asks the supervisor and mise, so the
/// deadline may end verification at any of these: readiness failed (`not_ready`, naming the
/// service), or a lookup was not started (`timed_out`) or was cut short (`provider_failed`).
/// The same lookups also run before the start, so the failed step must be `verify`, after it.
fn assert_cut_short_verifying(error: &Value, service: &str) {
    let cause = &error["details"][0]["cause"];
    let message = cause["message"].as_str().unwrap_or_default();
    let lookup = ["mise env --json", "mise daemons --json"].into_iter().any(|call| match cause["code"].as_str() {
        Some("timed_out") => message == format!("{call}: the deadline passed before it could start"),
        Some("provider_failed") => message == format!("{call} was cut short by the deadline"),
        _ => false,
    });
    let unready = cause["code"] == "not_ready" && cause["details"][0]["service"] == service;
    assert!(lookup || unready, "{error}");
    let progress = &error["details"][1];
    assert_eq!(progress["changed"], true, "{error}");
    let steps = progress["steps"].as_array().unwrap();
    assert_eq!(steps[steps.len() - 2], json!({ "step": "start", "status": "ok", "detail": null }), "{error}");
    assert_eq!(steps[steps.len() - 1], json!({ "step": "verify", "status": "failed", "code": cause["code"] }), "{error}");
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
    assert_cut_short_verifying(error, "web");

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
    mcp_up_times_out_and_down_stops(&fixture, &service);
}

#[test]
fn mcp_up_cut_short_while_verifying_keeps_what_launched_for_down() {
    let fixture = Fixture::with_bundle(WEB);
    let service = Detached::spawn();
    supervise_never_ready(&fixture, &service);
    fs::write(fixture.dir.path().join("slow-env-after-start"), "30").unwrap();
    let start = Instant::now();
    let cause = mcp_up_times_out_and_down_stops(&fixture, &service);
    assert_eq!(cause["code"], "provider_failed", "{cause}");
    assert_eq!(cause["message"], "mise env --json was cut short by the deadline", "{cause}");
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
}

/// `stack_up` of `service` (supervised, never ready) with a 2s timeout, then `stack_status` and
/// `stack_down` from the same server: the timeout is a tool error that leaves the launch
/// recorded and unhealthy, and `down` stops it. Returns the timeout's cause. On a loaded
/// machine 1s can pass before the start, when nothing was launched yet.
fn mcp_up_times_out_and_down_stops(fixture: &Fixture, service: &Detached) -> Value {
    let results = fixture.mcp(&[("stack_up", json!({ "timeout_secs": 2 })), ("stack_status", json!({})), ("stack_down", json!({}))], &[]);
    assert_eq!(results[0]["isError"], true, "{}", results[0]);
    let error = &results[0]["structuredContent"]["error"];
    assert_eq!(error["code"], "timed_out", "{error}");
    assert_eq!(error["message"], "`stack up` did not finish within 2s");
    assert!(error["hint"].as_str().unwrap().contains("`stack down` stops them"), "{error}");
    assert_cut_short_verifying(error, "web");
    let status = &results[1]["structuredContent"]["data"];
    assert_eq!(status["healthy"], false, "{status}");
    assert_eq!(status["session"]["launching"], true, "{status}");
    assert_eq!(status["session"]["services"]["web"]["pid"], service.0, "{status}");
    assert_eq!(results[2]["isError"], false, "{}", results[2]);
    assert!(!service.alive());
    assert!(fixture.session_paths().iter().all(|p| !p.exists()), "the launch is still recorded after down");
    error["details"][0]["cause"].clone()
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

// ---- Rust build caching through Mr Boxington (route 2) ---------------------------------------

const RUST_MBX: &str = "[bundle]\nname = 'rust-mbx'\n[tools]\nrust = { version = '1.93', mr_boxington = true }\nmbx = '1.22.0'\n[tasks.build]\nrun = 'cargo build'\n";

#[test]
fn resolution_sees_only_a_tools_configuration_in_a_scratch_root_of_its_own() {
    let fixture = Fixture::with_bundle(RUST_MBX);
    let app = fixture.dir.path().join("app");
    fs::write(app.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n[env]\nSECRET = \"{{ exec(command='touch ran') }}\"\n").unwrap();
    let _ = fs::remove_file(fixture.dir.path().join("latest.log"));
    fixture.ok(&["compile", "--update"]);
    let log = fs::read_to_string(fixture.dir.path().join("latest.log")).unwrap();
    let cache = fs::canonicalize(fixture.dir.path().join("cache")).unwrap();
    let dirs: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("dir=")).map(|l| l.split(' ').next().unwrap()).collect();
    assert_eq!(dirs.len(), 2, "rust and mbx: {log}");
    assert_ne!(dirs[0], dirs[1], "each resolution gets its own root: {log}");
    for dir in &dirs {
        assert!(Path::new(dir).starts_with(cache.join("resolve")), "{dir}");
        assert!(!Path::new(dir).exists(), "scratch roots are removed: {dir}");
        assert!(log.contains(&format!("dir={dir} trusted={dir} no_config=unset")) || {
            // The trusted path may be spelled through a symlink (/var vs /private/var).
            let line = log.lines().find(|l| l.starts_with(&format!("dir={dir} "))).unwrap();
            let trusted = line.split("trusted=").nth(1).unwrap().split(' ').next().unwrap();
            fs::canonicalize(Path::new(trusted).parent().unwrap()).unwrap() == Path::new(dir).parent().unwrap()
        }, "{log}");
    }
    assert!(log.contains("[tools.rust]\nversion = \"1.93\"\nmr_boxington = true\n"), "{log}");
    assert!(log.contains("[tools]\nmbx = \"1.22.0\"\n"), "{log}");
    assert!(!log.contains("[env]") && !log.contains("exec("), "{log}");
    assert!(!app.join("ran").exists());
    let lock = fs::read_to_string(app.join("stack.lock")).unwrap();
    assert!(lock.contains("resolved = \"1.93.1\"") && lock.contains("mr_boxington = true"), "{lock}");
}

#[test]
fn install_and_up_refuse_a_mise_too_old_for_mr_boxington_before_installing() {
    let fixture = Fixture::with_bundle(RUST_MBX);
    let log = fixture.dir.path().join("mise.log");
    fs::write(fixture.dir.path().join("mise-version"), "2026.9.1 macos-arm64 (2026-09-01)\n").unwrap();
    for args in [&["install", "--json"][..], &["up", "--json"]] {
        let _ = fs::remove_file(&log);
        let out = fixture.command(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        let err: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(err["error"]["code"], "provider_outdated", "{err}");
        assert!(err["error"]["message"].as_str().unwrap().contains("older than 2026.9.2"), "{err}");
        assert_eq!(err["error"]["details"][0]["actual"], "2026.9.1", "{err}");
        let calls = fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("install") && !calls.contains("trust"), "{args:?}: {calls}");
    }
    let version_log = fs::read_to_string(fixture.dir.path().join("version.log")).unwrap();
    assert!(version_log.lines().all(|l| l == "no_config=1"), "{version_log}");

    let out = fixture.command(&["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    let check = doctor["error"]["details"].as_array().unwrap().iter().find(|c| c["name"] == "mise_release").cloned();
    assert_eq!(check.as_ref().unwrap()["ok"], false, "{doctor}");

    // The release that introduced the option is enough; stderr's update notice is ignored.
    fs::write(fixture.dir.path().join("mise-version"), "2026.9.2 macos-arm64 (2026-09-02)\n").unwrap();
    let _ = fs::remove_file(&log);
    fixture.ok(&["install", "--json"]);
    assert!(fs::read_to_string(&log).unwrap().contains("install --yes --quiet"));
    let out = fixture.command(&["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = doctor["data"].as_array().or(doctor["error"]["details"].as_array()).unwrap().clone();
    let check = checks.iter().find(|c| c["name"] == "mise_release").unwrap();
    assert_eq!(check["ok"], true, "{doctor}");
    assert!(check["detail"].as_str().unwrap().starts_with("mise 2026.9.2; needs 2026.9.2 (tools.rust (bundle:rust-mbx) sets mr_boxington)"), "{doctor}");
}

#[test]
fn stacks_without_mr_boxington_never_ask_for_the_provider_release() {
    let fixture = Fixture::new();
    fs::write(fixture.dir.path().join("mise-version"), "2020.1.1\n").unwrap();
    fixture.ok(&["install", "--json"]);
    fixture.ok(&["up", "--json"]);
    let out = fixture.command(&["doctor", "--json"]).output().unwrap();
    let doctor: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!doctor.to_string().contains("mise_release"), "{doctor}");
    assert!(!fixture.dir.path().join("version.log").exists());
}

#[test]
fn exec_finds_cargo_through_the_wrapper_mise_puts_first_and_run_passes_through() {
    let fixture = Fixture::with_bundle(RUST_MBX);
    let wrappers = fixture.dir.path().join("command-wrappers/bin");
    fixture.ok(&["install"]);
    fs::create_dir_all(&wrappers).unwrap();
    fs::write(wrappers.join("cargo"), "#!/bin/sh\necho \"wrapped cargo $*\"\n").unwrap();
    fs::set_permissions(wrappers.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}:{}", wrappers.display(), fixture.dir.path().join("bin").display(), std::env::var("PATH").unwrap());
    fs::write(fixture.dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
    let out = fixture.ok(&["--json", "exec", "--", "cargo", "build"]);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["data"]["stdout"], "wrapped cargo build\n", "{result}");
    let out = fixture.ok(&["--json", "run", "build"]);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["data"]["stdout"], "build|", "the task goes to `mise run` unchanged: {result}");
    let rendered = fs::read_to_string(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap();
    let doc: toml::Table = toml::from_str(&rendered).unwrap();
    assert_eq!(doc["tools"]["rust"]["version"].as_str(), Some("1.93.1"), "{rendered}");
    assert_eq!(doc["tools"]["rust"]["mr_boxington"].as_bool(), Some(true), "{rendered}");
}

// ---- secret grants -----------------------------------------------------------------------

const DEPLOY_VALUE: &str = "leak-sentinel-deploy-0001";
/// Every value the fake fnox can print, on any stream; none may reach a result or a file.
const SENTINELS: &[&str] = &[
    DEPLOY_VALUE,
    "leak-sentinel-sentry-0002",
    "leak-sentinel-dependency-0003",
    "leak-sentinel-stderr-0004",
    "leak-sentinel-config-0005",
    "leak-sentinel-garbage-0006",
    "short77",
];

/// fnox as mise installs it: the executable in the release directory and a link to it in
/// `.mise-bins`, which is what the stack's PATH names. Steps answer from files the test writes
/// (`fnox-<step>-mode`, `fnox-<step>.json`); every run logs its arguments and prints a
/// diagnostic quoting a secret on stderr, as fnox does for a malformed configuration.
const FAKE_FNOX: &str = include_str!("fakes/fnox.sh");


fn write_exe(path: &Path, script: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A project with fnox 1.39.0 in its tools, installed (as mise reports it) under
/// `installs/fnox/1.39.0`, plus `extra` in stack.toml. The stack's PATH starts with the
/// `.mise-bins` directory of `path_install` (the pinned release unless a test says otherwise).
fn secrets_fixture(extra: &str) -> Fixture {
    let fixture = Fixture::new();
    let root = fixture.dir.path();
    let install = root.join("installs/fnox/1.39.0");
    write_exe(&install.join("fnox"), FAKE_FNOX);
    fs::create_dir_all(install.join(".mise-bins")).unwrap();
    std::os::unix::fs::symlink(install.join("fnox"), install.join(".mise-bins/fnox")).unwrap();
    installed(&fixture, &[("1.39.0", true)]);
    path_first(&fixture, &install.join(".mise-bins"));
    fs::write(
        root.join("app/stack.toml"),
        format!("[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n{extra}"),
    )
    .unwrap();
    fixture.ok(&["compile"]);
    fixture.ok(&["install"]);
    fixture
}

/// What `mise ls --json fnox` reports: these releases, installed or not.
fn installed(fixture: &Fixture, releases: &[(&str, bool)]) {
    let root = fixture.dir.path();
    let rows: Vec<Value> = releases
        .iter()
        .map(|(v, installed)| json!({ "version": v, "install_path": root.join("installs/fnox").join(v), "installed": installed, "active": true }))
        .collect();
    fs::write(root.join("ls.json"), Value::Array(rows).to_string()).unwrap();
}

/// The stack's PATH (as `mise env` reports it) with `dir` first.
fn path_first(fixture: &Fixture, dir: &Path) {
    let path = format!("{}:{}:{}", dir.display(), fixture.dir.path().join("bin").display(), std::env::var("PATH").unwrap());
    fs::write(fixture.dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
}

fn assert_no_leak(text: &str, context: &str) {
    for sentinel in SENTINELS {
        assert!(!text.contains(sentinel), "{context}: {sentinel} leaked into {text}");
    }
}

/// No sentinel in anything stack wrote: the project's `.stack/`, generated config, stack.lock,
/// the machine state and the cache.
fn assert_no_leak_on_disk(fixture: &Fixture) {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(&path, files);
            } else if kind.is_file() {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for dir in ["app", "state", "cache"] {
        walk(&fixture.dir.path().join(dir), &mut files);
    }
    assert!(files.iter().any(|f| f.ends_with("stack.lock")), "{files:?}");
    for file in files {
        let text = String::from_utf8_lossy(&fs::read(&file).unwrap()).into_owned();
        assert_no_leak(&text, &file.display().to_string());
    }
}

/// `stack --json <args>`: the envelope, and both streams for leak checks.
fn json_run(fixture: &Fixture, args: &[&str]) -> (Value, String, Output) {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = fixture.command(&full).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{args:?}: {text}"));
    (envelope, text, out)
}

fn fnox_log(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture.dir.path().join("fnox.log")).unwrap_or_default().lines().map(String::from).collect()
}

#[test]
fn a_grant_reaches_only_its_command_and_is_redacted_from_captured_output() {
    let fixture = secrets_fixture("[services.db]\npreset = 'postgres'\nversion = '17'\n");
    let script = r#"printf '%s\n' "$DEPLOY_KEY"; printf 'x%sy\n' "$DEPLOY_KEY" >&2; env | sort"#;
    let mut command = fixture.command(&["--json", "exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", script]);
    let out = command.env("HIDDEN", "inherited-hidden").env("DATABASE_URL", "postgresql://u@127.0.0.1:5432/db").output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{text}");
    assert_no_leak(&text, "exec --json");
    let data = &serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"];
    assert_eq!(data["secrets"], json!(["DEPLOY_KEY"]));
    let stdout = data["stdout"].as_str().unwrap();
    assert!(stdout.starts_with("[redacted:DEPLOY_KEY]\n"), "{stdout}");
    assert!(stdout.contains("\nDEPLOY_KEY=[redacted:DEPLOY_KEY]\n"), "{stdout}");
    assert_eq!(data["stderr"], "x[redacted:DEPLOY_KEY]y\n");
    // Unrequested keys and dependencies are dropped; fnox's removal applies where stack allows.
    for absent in ["SENTRY_DSN=", "SHORT_KEY=", "DEP=", "HIDDEN="] {
        assert!(!stdout.contains(&format!("\n{absent}")), "{absent} in {stdout}");
    }
    // Protected variables keep what stack decided (withheld, poisoned, set) and are reported.
    assert!(stdout.contains(&format!("\nPGHOST={UNVERIFIED}\n")), "{stdout}");
    assert!(stdout.contains(&format!("\nDATABASE_URL=postgresql://u@{UNVERIFIED}:5432/")), "{stdout}");
    assert!(stdout.contains("\nPATH=") && stdout.contains("\nSTACK_PROJECT="), "{stdout}");
    let warnings: Vec<&str> = data["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    for name in ["PGHOST", "DATABASE_URL", "PATH", "MISE_SHELL", "__MISE_DIFF", "STACK_PROJECT"] {
        assert!(warnings.contains(&format!("fnox asked to remove {name}; kept").as_str()), "{name}: {warnings:?}");
    }
    // Describe first, then only the granted keys; never interactive; in the project.
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let log = fnox_log(&fixture);
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0], format!("--non-interactive --no-daemon env --json --describe|1|{}", app.display()));
    assert_eq!(log[1], format!("--non-interactive --no-daemon env --json --keys DEPLOY_KEY|1|{}", app.display()));
    // The release was located in a scratch root that names fnox and nothing else, now gone.
    let ls = fs::read_to_string(fixture.dir.path().join("fnox-ls.log")).unwrap();
    assert!(ls.contains(&format!("dir={}", fixture.dir.path().join("cache/secrets").canonicalize().unwrap().display())), "{ls}");
    let config: String = ls.lines().filter(|l| !l.starts_with("dir=") && !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    assert_eq!(config.trim(), "[tools]\nfnox = \"1.39.0\"", "{ls}");
    assert_eq!(fs::read_dir(fixture.dir.path().join("cache/secrets")).unwrap().count(), 0);

    // A timed-out capture is redacted too, on the CLI and over MCP.
    let (envelope, text, out) = json_run(&fixture, &["exec", "--timeout", "1s", "--secret", "DEPLOY_KEY", "--", "sh", "-c", r#"echo "$DEPLOY_KEY"; sleep 5"#]);
    assert_eq!(out.status.code(), Some(124), "{text}");
    assert_eq!(envelope["error"]["code"], "timed_out");
    assert_eq!(envelope["error"]["details"][0]["stdout"], "[redacted:DEPLOY_KEY]\n");
    assert_no_leak(&text, "exec --json timeout");

    let results = fixture.mcp(
        &[
            ("stack_exec", json!({ "command": ["sh", "-c", script], "secrets": ["DEPLOY_KEY"] })),
            ("stack_exec", json!({ "command": ["sh", "-c", r#"echo "$DEPLOY_KEY"; sleep 5"#], "secrets": ["DEPLOY_KEY"], "timeout_secs": 1 })),
            ("stack_exec", json!({ "command": ["true"], "secrets": "DEPLOY_KEY" })),
            ("stack_exec", json!({ "command": ["true"], "secrets": [7] })),
            ("stack_exec", json!({ "command": ["true"], "secrets": ["PGHOST"] })),
        ],
        &[],
    );
    assert_no_leak(&results.iter().map(Value::to_string).collect::<String>(), "MCP");
    let data = &results[0]["structuredContent"]["data"];
    assert_eq!(data["secrets"], json!(["DEPLOY_KEY"]), "{}", results[0]);
    assert!(data["stdout"].as_str().unwrap().contains("DEPLOY_KEY=[redacted:DEPLOY_KEY]"));
    assert_eq!(results[1]["structuredContent"]["error"]["code"], "timed_out");
    assert_eq!(results[1]["structuredContent"]["error"]["details"][0]["stdout"], "[redacted:DEPLOY_KEY]\n");
    assert_eq!(results[2]["structuredContent"]["error"]["code"], "usage");
    assert_eq!(results[3]["structuredContent"]["error"]["code"], "usage");
    assert_eq!(results[4]["structuredContent"]["error"]["code"], "invalid_secret");

    // On the terminal the command owns its output: nothing is captured, so nothing is redacted.
    let out = fixture.command(&["exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", r#"printf %s "$DEPLOY_KEY""#]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), DEPLOY_VALUE);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("stack: fnox asked to remove PGHOST; kept"), "{stderr}");
    assert_no_leak(&stderr, "terminal stderr");

    // Inherited variables still pass through byte for byte alongside a grant.
    use std::os::unix::ffi::OsStrExt;
    let raw: &[u8] = b"raw\xff\xfebytes";
    let out = fixture
        .command(&["exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", DUMP_RAW])
        .env("UNRELATED_RAW", std::ffi::OsStr::from_bytes(raw))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(dumped(&fixture)["UNRELATED_RAW"], raw);
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn tasks_get_exactly_their_declared_secrets() {
    let fixture = secrets_fixture("[tasks.showenv]\nrun = 'env'\nsecrets = ['DEPLOY_KEY']\n[tasks.plain]\nrun = 'true'\n");
    let (envelope, text, out) = json_run(&fixture, &["run", "showenv"]);
    assert!(out.status.success(), "{text}");
    assert_no_leak(&text, "run --json");
    let data = &envelope["data"];
    assert_eq!(data["secrets"], json!(["DEPLOY_KEY"]));
    for stream in ["stdout", "stderr"] {
        let s = data[stream].as_str().unwrap();
        assert!(s.contains("\nDEPLOY_KEY=[redacted:DEPLOY_KEY]\n"), "{stream}: {s}");
        assert!(!s.contains("SENTRY_DSN="), "{stream}: {s}");
    }
    assert_eq!(fnox_log(&fixture).len(), 2);
    // A task without secrets runs as before: no fnox, no secrets fields.
    let (envelope, _, out) = json_run(&fixture, &["run", "plain"]);
    assert!(out.status.success());
    assert!(envelope["data"].get("secrets").is_none(), "{envelope}");
    assert_eq!(fnox_log(&fixture).len(), 2);

    let results = fixture.mcp(
        &[
            ("stack_run", json!({ "task": "showenv" })),
            ("stack_run", json!({ "task": "plain", "secrets": ["DEPLOY_KEY"] })),
            ("stack_inspect", json!({})),
        ],
        &[],
    );
    assert_no_leak(&results.iter().map(Value::to_string).collect::<String>(), "MCP");
    assert!(results[0]["structuredContent"]["data"]["stdout"].as_str().unwrap().contains("DEPLOY_KEY=[redacted:DEPLOY_KEY]"), "{}", results[0]);
    assert_eq!(results[1]["structuredContent"]["error"]["code"], "usage");
    assert_eq!(results[2]["structuredContent"]["data"]["stack"]["tasks"]["showenv"]["value"]["secrets"], json!(["DEPLOY_KEY"]));
    let (inspect, text, _) = json_run(&fixture, &["inspect"]);
    assert_eq!(inspect["data"]["stack"]["tasks"]["showenv"]["value"]["secrets"], json!(["DEPLOY_KEY"]));
    assert_no_leak(&text, "inspect");
    // The provider config never carries the grant: mise's own task `secrets` field is not used.
    let rendered = fs::read_to_string(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap();
    assert!(!rendered.contains("secrets") && !rendered.contains("DEPLOY_KEY"), "{rendered}");
    assert_no_leak_on_disk(&fixture);
}

/// A `stack run` (or MCP `stack_run`) started while another command holds the project lock.
/// Killed if the test fails before it finishes.
struct Queued(Option<std::process::Child>);

impl Queued {
    /// Start `command` while `lock` is held, and wait until it has had time to read the
    /// project, still blocked on the lock.
    fn start(mut command: Command, mcp_call: Option<Value>) -> Self {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        if let Some(call) = mcp_call {
            use std::io::Write;
            writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}})).unwrap();
            writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_run","arguments":call}})).unwrap();
        }
        drop(stdin);
        thread::sleep(Duration::from_millis(750));
        let mut queued = Self(Some(child));
        assert!(queued.0.as_mut().unwrap().try_wait().unwrap().is_none(), "the run must wait for the held project lock");
        queued
    }

    fn finish(mut self) -> Output {
        let mut child = self.0.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the queued run did not finish once the lock was released");
            }
            thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Hold the project lock as another stack command would, queue `stack run <task>` (CLI) or
/// `stack_run` (MCP) behind it, write `edited` as stack.toml while it waits, then release.
/// Returns the run's result envelope.
fn run_queued_behind_an_edit(fixture: &Fixture, task: &str, edited: &str, mcp: bool) -> Value {
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let held = stack::state::project_lock(&fixture.dir.path().join("state"), &app).unwrap();
    let queued = if mcp {
        Queued::start(fixture.command(&["mcp"]), Some(json!({ "dir": app, "task": task })))
    } else {
        Queued::start(fixture.command(&["--json", "run", task]), None)
    };
    fs::write(app.join("stack.toml"), edited).unwrap();
    drop(held);
    let out = queued.finish();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !mcp {
        return serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{stdout} {}", String::from_utf8_lossy(&out.stderr)));
    }
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let response = stdout.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|r| r["id"] == 1).unwrap();
    response["result"]["structuredContent"].clone()
}

#[test]
fn a_task_edited_while_its_run_waits_for_the_lock_runs_with_its_new_definition_and_grant() {
    let manifest = |body: &str, secret: &str| {
        format!("[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n[tasks.showenv]\nrun = '{body}'\nsecrets = ['{secret}']\n")
    };
    let (old, new) = (manifest("old-definition", "DEPLOY_KEY"), manifest("new-definition", "SENTRY_DSN"));
    let fixture = secrets_fixture("");
    let app = fixture.dir.path().join("app");
    for mcp in [false, true] {
        let context = if mcp { "MCP" } else { "CLI" };
        fs::write(app.join("stack.toml"), &old).unwrap();
        fixture.ok(&["compile"]);
        let before = fnox_log(&fixture).len();
        let envelope = run_queued_behind_an_edit(&fixture, "showenv", &new, mcp);
        assert_no_leak(&envelope.to_string(), context);
        let data = &envelope["data"];
        assert_eq!(envelope["ok"], true, "{context}: {envelope}");
        // The grant is the definition that ran, not the one read before the lock was free.
        assert_eq!(data["secrets"], json!(["SENTRY_DSN"]), "{context}: {envelope}");
        let stdout = data["stdout"].as_str().unwrap();
        assert!(stdout.contains("\nSENTRY_DSN=[redacted:SENTRY_DSN]\n"), "{context}: {stdout}");
        assert!(!stdout.contains("DEPLOY_KEY="), "{context}: the old grant reached the task: {stdout}");
        let ran = fs::read_to_string(fixture.dir.path().join("run-config")).unwrap();
        assert!(ran.contains("new-definition") && !ran.contains("old-definition"), "{context}: {ran}");
        let log = fnox_log(&fixture);
        assert_eq!(log.len(), before + 2, "{context}: {log:?}");
        assert!(log[before + 1].contains("--keys SENTRY_DSN|"), "{context}: {log:?}");
    }
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn a_task_deleted_or_ungranted_while_its_run_waits_is_refused_or_runs_without_secrets() {
    let head = "[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n";
    let granted = format!("{head}[tasks.showenv]\nrun = 'old-definition'\nsecrets = ['DEPLOY_KEY']\n[tasks.plain]\nrun = 'true'\n");
    let fixture = secrets_fixture("");
    let app = fixture.dir.path().join("app");
    let generated = app.join(".config/mise/conf.d/stack.toml");
    for mcp in [false, true] {
        let context = if mcp { "MCP" } else { "CLI" };

        // Deleted: refused as unknown, before the compile writes anything, fnox, or the task.
        fs::write(app.join("stack.toml"), &granted).unwrap();
        fixture.ok(&["compile"]);
        let config = fs::read_to_string(&generated).unwrap();
        let (fnox_before, mise_before) = (fnox_log(&fixture).len(), fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap());
        let deleted = format!("{head}[tasks.plain]\nrun = 'renamed'\n");
        let envelope = run_queued_behind_an_edit(&fixture, "showenv", &deleted, mcp);
        assert_eq!(envelope["error"]["code"], "unknown_task", "{context}: {envelope}");
        assert_eq!(envelope["error"]["hint"], "tasks: plain", "{context}");
        assert_eq!(fs::read_to_string(&generated).unwrap(), config, "{context}: the refused run wrote the generated config");
        assert_eq!(fnox_log(&fixture).len(), fnox_before, "{context}");
        let mise_log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
        assert!(!mise_log[mise_before.len()..].contains("run --skip-deps"), "{context}: {mise_log}");

        // Ungranted: runs its new definition with no secrets and no fnox call.
        fs::write(app.join("stack.toml"), &granted).unwrap();
        fixture.ok(&["compile"]);
        let fnox_before = fnox_log(&fixture).len();
        let ungranted = format!("{head}[tasks.showenv]\nrun = 'new-definition'\n");
        let envelope = run_queued_behind_an_edit(&fixture, "showenv", &ungranted, mcp);
        assert_eq!(envelope["ok"], true, "{context}: {envelope}");
        assert!(envelope["data"].get("secrets").is_none(), "{context}: {envelope}");
        assert!(!envelope["data"]["stdout"].as_str().unwrap().contains("DEPLOY_KEY="), "{context}: {envelope}");
        assert!(fs::read_to_string(fixture.dir.path().join("run-config")).unwrap().contains("new-definition"), "{context}");
        assert_eq!(fnox_log(&fixture).len(), fnox_before, "{context}: fnox ran for a task that declares no secrets");
    }
    assert_no_leak_on_disk(&fixture);
}

/// The copies of the provider configuration task runs hold (`<cache>/task-config/*`).
fn task_configs(fixture: &Fixture) -> Vec<std::path::PathBuf> {
    fs::read_dir(fixture.dir.path().join("cache/task-config"))
        .map(|entries| entries.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

/// Start `stack run <task>` (CLI) or `stack_run` (MCP) with the fake provider's gate held, and
/// return once `mise run` is waiting at it: stack has planned the task and released the project
/// lock, mise has not read its configuration. Then write `edited` as stack.toml and compile it
/// as any other command would, check the copy the run holds, open the gate and return the run's
/// result envelope.
fn run_gated_across_a_compile(fixture: &Fixture, task: &str, edited: &str, mcp: bool) -> Value {
    let root = fixture.dir.path();
    let app = root.join("app").canonicalize().unwrap();
    let (gate, waiting) = (root.join("run-gate"), root.join("run-waiting"));
    let _ = fs::remove_file(&waiting);
    fs::write(&gate, "").unwrap();
    let mut command = if mcp { fixture.command(&["mcp"]) } else { fixture.command(&["--json", "run", task]) };
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if mcp {
        use std::io::Write;
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}})).unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_run","arguments":{"dir": app, "task": task}}})).unwrap();
    }
    drop(stdin);
    let queued = Queued(Some(child));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !waiting.exists() {
        assert!(Instant::now() < deadline, "mise run never reached the gate");
        thread::sleep(Duration::from_millis(20));
    }
    // The copy the planned run holds: one private directory, the definition it was planned
    // from, and nothing granted.
    let copies = task_configs(fixture);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(fs::metadata(&copies[0]).unwrap().permissions().mode() & 0o777, 0o700);
    let held = fs::read_to_string(copies[0].join(".config/mise/conf.d/stack.toml")).unwrap();
    assert_eq!(held, fs::read_to_string(app.join(".config/mise/conf.d/stack.toml")).unwrap());
    assert_no_leak(&held, "the task's configuration copy");
    // The lock is free: an ordinary compile publishes the edit while the run waits.
    fs::write(app.join("stack.toml"), edited).unwrap();
    fixture.ok(&["compile"]);
    assert_ne!(fs::read_to_string(app.join(".config/mise/conf.d/stack.toml")).unwrap(), held, "the compile must change the generated config");
    fs::remove_file(&gate).unwrap();
    let out = queued.finish();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !mcp {
        return serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{stdout} {}", String::from_utf8_lossy(&out.stderr)));
    }
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let response = stdout.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|r| r["id"] == 1).unwrap();
    response["result"]["structuredContent"].clone()
}

#[test]
fn a_planned_task_runs_the_definition_and_grant_it_was_planned_with_whatever_is_compiled_before_mise_starts() {
    let head = "[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n";
    let manifest = |body: &str, secret: &str| format!("{head}[tasks.showenv]\nrun = '{body}'\nsecrets = ['{secret}']\n[tasks.other]\nrun = 'other'\n");
    let (old, new) = (manifest("old-definition", "DEPLOY_KEY"), manifest("new-definition", "SENTRY_DSN"));
    let deleted = format!("{head}[tasks.other]\nrun = 'other'\n");
    let fixture = secrets_fixture("");
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    for mcp in [false, true] {
        for (edit, edited) in [("edited", &new), ("deleted", &deleted)] {
            let context = format!("{} {edit}", if mcp { "MCP" } else { "CLI" });
            fs::write(app.join("stack.toml"), &old).unwrap();
            fixture.ok(&["compile"]);
            let envelope = run_gated_across_a_compile(&fixture, "showenv", edited, mcp);
            assert_no_leak(&envelope.to_string(), &context);
            assert_eq!(envelope["ok"], true, "{context}: {envelope}");
            let data = &envelope["data"];
            // The body and the grant both come from the plan, not from the later compile.
            let ran = fs::read_to_string(fixture.dir.path().join("run-config")).unwrap();
            assert!(ran.contains("old-definition") && !ran.contains("new-definition"), "{context}: {ran}");
            assert_eq!(data["secrets"], json!(["DEPLOY_KEY"]), "{context}: {envelope}");
            let stdout = data["stdout"].as_str().unwrap();
            assert!(stdout.contains("\nDEPLOY_KEY=[redacted:DEPLOY_KEY]\n"), "{context}: {stdout}");
            assert!(!stdout.contains("SENTRY_DSN="), "{context}: {stdout}");
            // mise loaded the copy as its only configuration, rooted at the project, from it.
            let run_log = fs::read_to_string(fixture.dir.path().join("run.log")).unwrap();
            let last = run_log.lines().last().unwrap();
            let copies = std::fs::canonicalize(fixture.dir.path().join("cache/task-config")).unwrap();
            assert!(last.starts_with(&format!("config={}/", copies.display())), "{context}: {last}");
            assert!(last.ends_with(&format!("/.config/mise/conf.d/stack.toml root={} dir={}", app.display(), app.display())), "{context}: {last}");
            assert!(stdout.contains(&format!("\nMISE_GLOBAL_CONFIG_ROOT={}\n", app.display())), "{context}: {stdout}");
            assert!(task_configs(&fixture).is_empty(), "{context}: the copy outlived its run");

            // Planned after the compile: what it published.
            let (envelope, text, _) = json_run(&fixture, &["run", "showenv"]);
            assert_no_leak(&text, &context);
            if edit == "edited" {
                assert_eq!(envelope["data"]["secrets"], json!(["SENTRY_DSN"]), "{context}: {envelope}");
                let stdout = envelope["data"]["stdout"].as_str().unwrap();
                assert!(stdout.contains("\nSENTRY_DSN=[redacted:SENTRY_DSN]\n") && !stdout.contains("DEPLOY_KEY="), "{context}: {stdout}");
                assert!(fs::read_to_string(fixture.dir.path().join("run-config")).unwrap().contains("new-definition"), "{context}");
            } else {
                assert_eq!(envelope["error"]["code"], "unknown_task", "{context}: {envelope}");
            }
        }
    }
    assert!(task_configs(&fixture).is_empty());
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn a_task_configuration_copy_is_removed_after_a_timeout_a_failed_plan_and_a_terminal_run() {
    let fixture = secrets_fixture("[tasks.hang]\nrun = 'sleep'\n[tasks.showenv]\nrun = 'x'\nsecrets = ['DEPLOY_KEY']\n");
    // Timed out: the run's process group is killed, then the copy removed.
    let (envelope, text, out) = json_run(&fixture, &["run", "--timeout", "1s", "hang"]);
    assert_eq!(out.status.code(), Some(124), "{text}");
    assert_eq!(envelope["error"]["code"], "timed_out", "{text}");
    assert!(task_configs(&fixture).is_empty(), "CLI timeout");
    let results = fixture.mcp(&[("stack_run", json!({ "task": "hang", "timeout_secs": 1 }))], &[]);
    assert_eq!(results[0]["structuredContent"]["error"]["code"], "timed_out", "{}", results[0]);
    assert!(task_configs(&fixture).is_empty(), "MCP timeout");
    // Planning fails after the copy was made: fnox cannot answer for the grant.
    fs::write(fixture.dir.path().join("fnox-keys-mode"), "exit").unwrap();
    let (envelope, text, _) = json_run(&fixture, &["run", "showenv"]);
    assert_eq!(envelope["ok"], false, "{text}");
    assert_no_leak(&text, "failed plan");
    let results = fixture.mcp(&[("stack_run", json!({ "task": "showenv" }))], &[]);
    assert_eq!(results[0]["structuredContent"]["ok"], false, "{}", results[0]);
    // Only the two timed-out runs reached mise; the failed plans did not.
    assert_eq!(fs::read_to_string(fixture.dir.path().join("run.log")).unwrap().lines().count(), 2);
    assert!(task_configs(&fixture).is_empty(), "failed plan");
    fs::remove_file(fixture.dir.path().join("fnox-keys-mode")).unwrap();
    // On the terminal: the copy is removed once the task exits, with the link mise made to it
    // and the one left by a run killed before it could clean up. Others are kept.
    let state = fixture.dir.path().join("mise-state");
    let tracked = state.join("tracked-configs");
    fs::create_dir_all(&tracked).unwrap();
    fs::write(fixture.dir.path().join("track-configs"), "").unwrap();
    let copies = fixture.dir.path().join("cache/task-config").canonicalize().unwrap();
    std::os::unix::fs::symlink(copies.join("999999999-killed/.config/mise/conf.d/stack.toml"), tracked.join("killed")).unwrap();
    let project_config = fixture.dir.path().join("app/.config/mise/conf.d/stack.toml");
    std::os::unix::fs::symlink(&project_config, tracked.join("project")).unwrap();
    let out = fixture.command(&["run", "showenv"]).env("MISE_STATE_DIR", &state).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(task_configs(&fixture).is_empty(), "terminal run");
    let run_log = fs::read_to_string(fixture.dir.path().join("run.log")).unwrap();
    assert!(run_log.lines().last().unwrap().starts_with(&format!("config={}/", copies.display())), "{run_log}");
    let left: Vec<_> = fs::read_dir(&tracked).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, ["project"], "mise's links to removed copies must go with them");
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn short_values_are_refused_when_captured_and_allowed_on_the_terminal() {
    let fixture = secrets_fixture("");
    let marker = fixture.dir.path().join("ran");
    let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "SHORT_KEY", "--", "touch", marker.to_str().unwrap()]);
    assert_eq!(envelope["error"]["code"], "secret_unsupported", "{text}");
    assert_eq!(envelope["error"]["details"][0]["key"], "SHORT_KEY");
    assert!(!marker.exists());
    assert_no_leak(&text, "exec --json short");
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["true"], "secrets": ["SHORT_KEY"] }))], &[]);
    assert_eq!(results[0]["structuredContent"]["error"]["code"], "secret_unsupported");
    let out = fixture.command(&["exec", "--secret", "SHORT_KEY", "--", "sh", "-c", r#"printf %s "$SHORT_KEY""#]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "short77");
    assert_no_leak_on_disk(&fixture);
}

/// fnox answers `--keys` with `set`, whatever keys were asked for.
fn answer_keys(fixture: &Fixture, set: Value) {
    let root = fixture.dir.path();
    fs::write(root.join("fnox-keys-mode"), "file").unwrap();
    fs::write(root.join("fnox-keys.json"), json!({ "schema": 1, "set": set, "files": {}, "remove": [], "missing": [], "leases": [] }).to_string()).unwrap();
}

/// No `values` in either captured stream of `data`, for every way of returning a result.
fn assert_streams_clean(data: &Value, values: &[&str], context: &str) {
    for stream in ["stdout", "stderr"] {
        let text = data[stream].as_str().unwrap_or_else(|| panic!("{context}: no {stream} in {data}"));
        for value in values {
            assert!(!text.contains(value), "{context}: {value} in {stream}: {text}");
        }
    }
}

#[test]
fn a_redaction_marker_never_reproduces_a_granted_value() {
    let fixture = secrets_fixture("");
    let marker = fixture.dir.path().join("ran");

    // The marker word: refused before the command runs, on the CLI and over MCP.
    answer_keys(&fixture, json!({ "DEPLOY_KEY": "redacted" }));
    let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "touch", marker.to_str().unwrap()]);
    assert_eq!(envelope["error"]["code"], "secret_unsupported", "{text}");
    assert_eq!(envelope["error"]["details"][0]["key"], "DEPLOY_KEY");
    assert!(!marker.exists(), "the command ran");
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["touch", marker.to_str().unwrap()], "secrets": ["DEPLOY_KEY"] }))], &[]);
    assert_eq!(results[0]["structuredContent"]["error"]["code"], "secret_unsupported", "{}", results[0]);
    assert!(!marker.exists(), "the command ran");
    // The terminal captures nothing, so the value is granted there.
    let out = fixture.command(&["exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", r#"printf %s "$DEPLOY_KEY""#]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "redacted");

    // SENTRY_DSN's value is what DEPLOY_KEY's labeled marker and the text after it would spell,
    // and DEPLOY_KEY's value is its own name: neither key is shown, and neither value appears.
    let tail = "d:DEPLOY_KEY]-tail-0007";
    answer_keys(&fixture, json!({ "DEPLOY_KEY": DEPLOY_VALUE, "SENTRY_DSN": tail }));
    let script = r#"printf '%s-tail-0007\n' "$DEPLOY_KEY"; printf '%s-tail-0007\n' "$DEPLOY_KEY" >&2; printf '%s\n' "$SENTRY_DSN""#;
    let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--secret", "SENTRY_DSN", "--", "sh", "-c", script]);
    assert!(out.status.success(), "{text}");
    assert_streams_clean(&envelope["data"], &[DEPLOY_VALUE, tail], "exec --json");
    assert_eq!(envelope["data"]["stdout"], "[redacted]-tail-0007\n[redacted:SENTRY_DSN]\n");
    assert_eq!(envelope["data"]["stderr"], "[redacted]-tail-0007\n");
    assert_no_leak(&text, "exec --json");
    let results = fixture.mcp(
        &[
            ("stack_exec", json!({ "command": ["sh", "-c", script], "secrets": ["DEPLOY_KEY", "SENTRY_DSN"] })),
            ("stack_exec", json!({ "command": ["sh", "-c", format!("{script}; sleep 5")], "secrets": ["DEPLOY_KEY", "SENTRY_DSN"], "timeout_secs": 1 })),
        ],
        &[],
    );
    assert_streams_clean(&results[0]["structuredContent"]["data"], &[DEPLOY_VALUE, tail], "MCP");
    assert_eq!(results[1]["structuredContent"]["error"]["code"], "timed_out", "{}", results[1]);
    assert_streams_clean(&results[1]["structuredContent"]["error"]["details"][0], &[DEPLOY_VALUE, tail], "MCP timeout");

    answer_keys(&fixture, json!({ "DEPLOY_KEY": "DEPLOY_KEY" }));
    let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", r#"printf '%s\n' "$DEPLOY_KEY" | tee /dev/stderr"#]);
    assert!(out.status.success(), "{text}");
    assert_eq!((envelope["data"]["stdout"].as_str(), envelope["data"]["stderr"].as_str()), (Some("[redacted]\n"), Some("[redacted]\n")));

    // A dependency named after a granted value is replaced without its name.
    answer_keys(&fixture, json!({ "DEPLOY_KEY": "LEAKSENTINEL0008", "DEP_LEAKSENTINEL0008": "leak-sentinel-dependency-0003" }));
    let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "sh", "-c", r#"printf '%s leak-sentinel-dependency-0003\n' "$DEPLOY_KEY""#]);
    assert!(out.status.success(), "{text}");
    assert_eq!(envelope["data"]["stdout"], "[redacted:DEPLOY_KEY] [redacted]\n");
    assert_streams_clean(&envelope["data"], &["LEAKSENTINEL0008", "leak-sentinel-dependency-0003"], "dependency");

    // The 64 KiB bound applies to the replaced stream: markers longer than the value they
    // replace do not let it grow, and a cut through a marker spells nothing.
    answer_keys(&fixture, json!({ "DEPLOY_KEY": DEPLOY_VALUE, "SENTRY_DSN": tail }));
    let script = r#"i=0; while [ $i -lt 4000 ]; do printf '%s-tail-0007' "$DEPLOY_KEY"; i=$((i+1)); done"#;
    let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--secret", "SENTRY_DSN", "--", "sh", "-c", script]);
    assert!(out.status.success(), "{text}");
    let stdout = envelope["data"]["stdout"].as_str().unwrap();
    let body = stdout.strip_prefix("…[truncated]…").unwrap_or_else(|| panic!("not truncated: {} bytes", stdout.len()));
    assert_eq!(body.len(), 64 * 1024);
    assert!(body.ends_with("[redacted]-tail-0007"), "{}", &body[body.len() - 40..]);
    assert_streams_clean(&envelope["data"], &[DEPLOY_VALUE, tail], "bounded");
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn fnox_failures_map_to_codes_and_nothing_fnox_printed_is_forwarded() {
    let fixture = secrets_fixture("");
    let root = fixture.dir.path().to_path_buf();
    let marker = root.join("ran");
    let describe = |keys: &str, leases: &str| {
        format!(r#"{{"schema":1,"fnox_version":"1.39.0","keys":[{keys}],"dynamic_leases":[{leases}]}}"#)
    };
    let ok_key = r#"{"key":"DEPLOY_KEY","kind":"secret","env":true,"as_file":false,"injectable":{"exec":true,"shell":true}}"#;
    // (step, mode, answer file, expected code, detail field=value, fnox runs)
    type Case<'a> = (&'a str, &'a str, Option<String>, &'a str, &'a str, usize);
    let cases: Vec<Case> = vec![
        ("describe", "garbage", None, "secret_unavailable", "kind=protocol", 1),
        ("describe", "config", None, "secret_unavailable", "kind=config", 1),
        ("describe", "oversized", None, "secret_unavailable", "kind=oversized", 1),
        ("describe", "exit", None, "secret_unavailable", "kind=protocol", 1),
        ("describe", "file", Some(describe(&ok_key.replace(r#""as_file":false"#, r#""as_file":true"#), "")), "secret_unsupported", "key=DEPLOY_KEY", 1),
        ("describe", "file", Some(describe(&ok_key.replace(r#""kind":"secret""#, r#""kind":"lease","lease":"aws""#), "")), "secret_unsupported", "key=DEPLOY_KEY", 1),
        ("describe", "file", Some(describe(&ok_key.replace(r#""exec":true"#, r#""exec":false"#), "")), "secret_unsupported", "key=DEPLOY_KEY", 1),
        ("describe", "file", Some(describe("", "")), "secret_missing", "reason=unknown", 1),
        ("keys", "garbage", None, "secret_unavailable", "kind=protocol", 2),
        ("keys", "config", None, "secret_unavailable", "kind=config", 2),
        ("keys", "oversized", None, "secret_unavailable", "kind=oversized", 2),
        ("keys", "exit", None, "secret_unavailable", "kind=protocol", 2),
        ("keys", "file", Some(r#"{"schema":1,"error":{"kind":"invalid_keys","message":"leak-sentinel-config-0005","unknown":["DEPLOY_KEY"]}}"#.into()), "secret_missing", "reason=unknown", 2),
        ("keys", "file", Some(r#"{"schema":1,"set":{},"files":{},"remove":[],"missing":["DEPLOY_KEY"],"leases":[]}"#.into()), "secret_missing", "reason=unresolved", 2),
        ("keys", "file", Some(r#"{"schema":1,"set":{"DEPLOY_KEY":"leak-sentinel-deploy-0001","PGHOST":"leak-sentinel-garbage-0006"},"files":{},"remove":[],"missing":[],"leases":[]}"#.into()), "invalid_secret", "operation=set", 2),
        ("keys", "file", Some(r#"{"schema":1,"set":{"DEPLOY_KEY":"leak-sentinel-deploy-0001"},"files":{"DEPLOY_KEY":"/x"},"remove":[],"missing":[],"leases":[]}"#.into()), "secret_unsupported", "key=DEPLOY_KEY", 2),
        ("keys", "file", Some(r#"{"schema":1,"set":{"DEPLOY_KEY":["leak-sentinel-deploy-0001"]}}"#.into()), "secret_unavailable", "kind=protocol", 2),
    ];
    for (step, mode, body, code, detail, runs) in cases {
        for f in ["fnox.log", "fnox-describe-mode", "fnox-keys-mode", "fnox-describe.json", "fnox-keys.json"] {
            let _ = fs::remove_file(root.join(f));
        }
        fs::write(root.join(format!("fnox-{step}-mode")), mode).unwrap();
        if let Some(body) = &body {
            fs::write(root.join(format!("fnox-{step}.json")), body).unwrap();
            fs::write(root.join(format!("fnox-{step}-exit")), if body.contains("\"error\"") { "1" } else { "0" }).unwrap();
        }
        let context = format!("{step} {mode} {}", body.as_deref().unwrap_or(""));
        let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "touch", marker.to_str().unwrap()]);
        assert!(!out.status.success(), "{context}: {text}");
        assert_eq!(envelope["error"]["code"], code, "{context}: {text}");
        let (field, value) = detail.split_once('=').unwrap();
        assert_eq!(envelope["error"]["details"][0][field], value, "{context}: {text}");
        assert_no_leak(&text, &context);
        assert!(!marker.exists(), "{context}: the command ran");
        assert_eq!(fnox_log(&fixture).len(), runs, "{context}");
        // People see the same error on stderr, equally clean.
        let out = fixture.command(&["exec", "--secret", "DEPLOY_KEY", "--", "touch", marker.to_str().unwrap()]).output().unwrap();
        let human = String::from_utf8_lossy(&out.stderr);
        assert!(human.contains(&format!("error[{code}]")), "{context}: {human}");
        assert_no_leak(&human, &context);
    }
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn only_the_fnox_release_stack_lock_pins_is_run() {
    let fixture = secrets_fixture("");
    let root = fixture.dir.path().to_path_buf();
    let impostor = format!("#!/bin/sh\ntouch \"$REVIEW_FIXTURE/impostor-ran\"\n{}", FAKE_FNOX.trim_start_matches("#!/bin/sh\n"));
    let refused = |context: &str, kind: &str| {
        let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "true"]);
        assert_eq!(envelope["error"]["code"], "secret_unavailable", "{context}: {text}");
        assert_eq!(envelope["error"]["details"][0]["kind"], kind, "{context}: {text}");
        assert!(!root.join("impostor-ran").exists(), "{context}: the impostor ran");
        assert!(fnox_log(&fixture).is_empty(), "{context}");
        assert_no_leak(&text, context);
    };
    // Another release's directory first on PATH.
    write_exe(&root.join("installs/fnox/1.38.0/.mise-bins/fnox"), &impostor);
    path_first(&fixture, &root.join("installs/fnox/1.38.0/.mise-bins"));
    refused("other release", "not_pinned");
    // Another executable beside the pinned directory.
    write_exe(&root.join("installs/fnox/evil/fnox"), &impostor);
    path_first(&fixture, &root.join("installs/fnox/evil"));
    refused("sibling directory", "not_pinned");
    // A link inside the pinned directory to a file outside it.
    let pinned_bins = root.join("installs/fnox/1.39.0/.mise-bins");
    write_exe(&root.join("elsewhere/fnox"), &impostor);
    fs::remove_file(pinned_bins.join("fnox")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere/fnox"), pinned_bins.join("fnox")).unwrap();
    path_first(&fixture, &pinned_bins);
    refused("link out of the pinned directory", "not_pinned");
    fs::remove_file(pinned_bins.join("fnox")).unwrap();
    std::os::unix::fs::symlink(root.join("installs/fnox/1.39.0/fnox"), pinned_bins.join("fnox")).unwrap();
    // The pinned release is not installed: exec refuses before any secret is looked up.
    let not_installed = |context: &str| {
        let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "true"]);
        assert_eq!(envelope["error"]["code"], "tools_not_installed", "{context}: {text}");
        assert_eq!(envelope["error"]["details"], json!([{ "tool": "fnox", "version": "1.39.0" }]), "{context}: {text}");
        assert!(!root.join("impostor-ran").exists(), "{context}: the impostor ran");
        assert!(fnox_log(&fixture).is_empty(), "{context}");
    };
    installed(&fixture, &[("1.39.0", false), ("1.38.0", true)]);
    not_installed("not installed");
    installed(&fixture, &[("1.38.0", true)]);
    not_installed("only another release installed");
    // Or not on PATH at all.
    installed(&fixture, &[("1.39.0", true)]);
    path_first(&fixture, &root.join("nowhere"));
    refused("not on PATH", "not_on_path");
    // Restored, it runs.
    path_first(&fixture, &pinned_bins);
    let (envelope, text, out) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "true"]);
    assert!(out.status.success(), "{text}");
    assert_eq!(envelope["data"]["secrets"], json!(["DEPLOY_KEY"]));
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn secret_declarations_are_checked_when_the_stack_compiles() {
    let fixture = secrets_fixture("");
    let app = fixture.dir.path().join("app");
    let compile = |stack: &str, bundle: &str| {
        fs::write(fixture.dir.path().join("bundle/bundle.toml"), format!("[bundle]\nname='test'\n{bundle}")).unwrap();
        fs::write(app.join("stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n{stack}")).unwrap();
        json_run(&fixture, &["compile"]).0
    };
    let fnox = "[tools]\nfnox = \"1.39.0\"\n";
    // A bundle may declare names; the project provides fnox.
    let ok = compile(fnox, "[tasks.deploy]\nrun = 'x'\nsecrets = ['DEPLOY_KEY']\n");
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["data"]["stack"]["tasks"]["deploy"]["value"]["secrets"], json!(["DEPLOY_KEY"]));
    for (stack, bundle, key) in [
        (format!("{fnox}[services.db]\npreset='postgres'\nversion='17'\n[tasks.t]\nrun='x'\nsecrets=['DATABASE_URL']\n"), "", "DATABASE_URL"),
        (format!("{fnox}[services.api]\nrun='x'\n[tasks.t]\nrun='x'\nsecrets=['API_PORT']\n"), "", "API_PORT"),
        (format!("{fnox}[tasks.t]\nrun='x'\nsecrets=['lower']\n"), "", "lower"),
        (format!("{fnox}[env]\nAPP_KEY='1'\n[tasks.t]\nrun='x'\nsecrets=['APP_KEY']\n"), "", "APP_KEY"),
        (format!("{fnox}[tasks.t]\nrun='x'\nsecrets=['MISE_ENV']\n"), "", "MISE_ENV"),
        (format!("{fnox}[override.tasks.t]\nrun='x'\nsecrets=['STACK_SESSION']\n"), "[tasks.t]\nrun='y'\n", "STACK_SESSION"),
        ("[tasks.t]\nrun='x'\nsecrets=['DEPLOY_KEY']\n".to_string(), "", "DEPLOY_KEY"),
        ("[tools]\nfnox='system'\n[tasks.t]\nrun='x'\nsecrets=['DEPLOY_KEY']\n".to_string(), "", "DEPLOY_KEY"),
        (fnox.to_string(), "[tasks.t]\nrun='x'\nsecrets=['DEPLOY_KEY', 'DEPLOY_KEY']\n", "DEPLOY_KEY"),
    ] {
        let failed = compile(&stack, bundle);
        assert_eq!(failed["error"]["code"], "invalid_secret", "{stack}{bundle}: {failed}");
        assert_eq!(failed["error"]["details"][0]["key"], key, "{stack}{bundle}: {failed}");
        assert_eq!(failed["error"]["details"][0]["operation"], "declare");
    }
    // Two layers that disagree on a task's secrets conflict, as on any other field.
    let conflict = compile(&format!("{fnox}[tasks.t]\nrun='x'\nsecrets=['A_KEY']\n"), "[tasks.t]\nrun='x'\nsecrets=['B_KEY']\n");
    assert_eq!(conflict["error"]["code"], "conflict", "{conflict}");
    // A command's grant is checked before any provider call answers it.
    compile(fnox, "");
    for key in ["PATH", "bad", "STACK_PROJECT"] {
        let (envelope, _, _) = json_run(&fixture, &["exec", "--secret", key, "--", "true"]);
        assert_eq!(envelope["error"]["code"], "invalid_secret", "{key}: {envelope}");
    }
    assert!(!fixture.dir.path().join("fnox-ls.log").exists());
    assert!(fnox_log(&fixture).is_empty());
}

#[test]
fn doctor_reports_fnox_from_its_value_free_description_only() {
    let fixture = secrets_fixture("[tasks.deploy]\nrun = 'x'\nsecrets = ['DEPLOY_KEY']\n");
    let fnox_check = |envelope: &Value| -> Value {
        let checks = if envelope["ok"] == true { &envelope["data"] } else { &envelope["error"]["details"] };
        checks.as_array().unwrap().iter().find(|c| c["name"] == "fnox").cloned().unwrap_or_else(|| panic!("{envelope}"))
    };
    let (envelope, text, _) = json_run(&fixture, &["doctor"]);
    let check = fnox_check(&envelope);
    assert_eq!(check["ok"], true, "{text}");
    assert!(check["detail"].as_str().unwrap().contains("fnox 1.39.0 at "), "{check}");
    assert_no_leak(&text, "doctor");
    let log = fnox_log(&fixture);
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("--non-interactive --no-daemon env --json --describe|1|"), "{log:?}");

    fs::write(fixture.dir.path().join("fnox-describe-mode"), "config").unwrap();
    let (envelope, text, _) = json_run(&fixture, &["doctor"]);
    assert_eq!(envelope["error"]["code"], "doctor_failed", "{text}");
    let check = fnox_check(&envelope);
    assert_eq!(check["ok"], false);
    assert!(check["detail"].as_str().unwrap().contains("config error"), "{check}");
    assert_no_leak(&text, "doctor with a malformed fnox config");

    fs::write(fixture.dir.path().join("fnox-describe-mode"), "file").unwrap();
    fs::write(fixture.dir.path().join("fnox-describe.json"), r#"{"schema":1,"keys":[],"dynamic_leases":[]}"#).unwrap();
    let (envelope, text, _) = json_run(&fixture, &["doctor"]);
    let check = fnox_check(&envelope);
    assert_eq!(check["ok"], false, "{text}");
    assert!(check["detail"].as_str().unwrap().contains("does not know DEPLOY_KEY"), "{check}");

    installed(&fixture, &[("1.39.0", false)]);
    let (envelope, _, _) = json_run(&fixture, &["doctor"]);
    let check = fnox_check(&envelope);
    assert_eq!(check["ok"], true);
    assert!(check["detail"].as_str().unwrap().contains("not installed"), "{check}");
    assert!(fnox_log(&fixture).iter().all(|l| !l.contains("--keys")), "doctor resolved a value");
    assert_no_leak_on_disk(&fixture);
}

#[test]
fn templated_fnox_pins_never_reach_the_release_query() {
    let fixture = secrets_fixture("");
    let root = fixture.dir.path().to_path_buf();
    let marker = root.join("template-ran");
    let template = format!("{{{{ exec(command='touch {}') }}}}", marker.display());
    // A stack.lock whose fnox pin was edited to a template is refused before any provider call.
    let lock_path = root.join("app/stack.lock");
    let lock = fs::read_to_string(&lock_path).unwrap();
    assert!(lock.contains("resolved = \"1.39.0\""), "{lock}");
    fs::write(&lock_path, lock.replace("resolved = \"1.39.0\"", &format!("resolved = {:?}", template))).unwrap();
    let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "true"]);
    assert_eq!(envelope["error"]["code"], "lock_invalid", "{text}");
    fs::write(&lock_path, &lock).unwrap();
    // An option carrying a template is refused: `invalid_tool` at compile once tool options are
    // checked for templates, and in any case never written into the fnox release query.
    fs::write(
        root.join("app/stack.toml"),
        format!("[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = {{ version = \"1.39.0\", identity = {:?} }}\n", template),
    )
    .unwrap();
    let (compiled, text, _) = json_run(&fixture, &["compile"]);
    if compiled["ok"] == true {
        let (envelope, text, _) = json_run(&fixture, &["exec", "--secret", "DEPLOY_KEY", "--", "true"]);
        assert_eq!(envelope["error"]["code"], "secret_unavailable", "{text}");
        assert_eq!(envelope["error"]["details"][0]["kind"], "templated", "{text}");
    } else {
        assert_eq!(compiled["error"]["code"], "invalid_tool", "{text}");
    }
    assert!(!fs::read_to_string(root.join("ls.log")).unwrap_or_default().contains("exec("), "a template reached `mise ls`");
    assert!(fnox_log(&fixture).is_empty());
    assert!(!marker.exists());
}

// ---- artifact locking ----------------------------------------------------------------------

const TOOLS_BUNDLE: &str = "[bundle]\nname='test'\n[tools]\njq='1.7.1'\nrust='1.93.1'\n'npm:prettier'='3.6.2'\nuv='0.9.0'\n[env]\nHOOK=\"{{ exec(command='touch hook-ran') }}\"\n";

/// A lock `mise lock` hands back: jq checked on this platform, rust exempt, prettier with a
/// sidecar reference, uv not lockable.
fn tool_lock(platform: &str) -> String {
    format!(
        r#"lockfile_version = 3
[[tools.jq]]
version = "1.7.1"
backend = "aqua:jqlang/jq"
specifiers = ["1.7.1"]
[tools.jq."platforms.{platform}"]
checksum = "sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a"
url = "https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64"
[[tools.rust]]
version = "1.93.1"
backend = "core:rust"
specifiers = ["1.93.1"]
[[tools."npm:prettier"]]
version = "3.6.2"
aube = {{ path = "locks/npm-prettier/3.6.2", digest = "sha256:0b89" }}
backend = "npm:prettier"
specifiers = ["3.6.2"]
"#
    )
}

/// The project locked for this platform only, with `extra` appended to stack.toml.
fn artifact_fixture(extra: &str) -> (Fixture, String) {
    let fixture = Fixture::with_bundle(TOOLS_BUNDLE);
    let platform = stack::artifacts::current_platform();
    fs::write(fixture.dir.path().join("mise-lock.toml"), tool_lock(&platform)).unwrap();
    fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n[lock]\nplatforms=['current']\n{extra}")).unwrap();
    fixture.ok(&["compile"]);
    (fixture, platform)
}

fn mise_log(fixture: &Fixture) -> String {
    fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap_or_default()
}

#[test]
fn compile_locks_in_a_tools_only_scratch_root_and_install_partitions_by_coverage() {
    let (fixture, platform) = artifact_fixture("");
    let lock_log = fs::read_to_string(fixture.dir.path().join("lock.log")).unwrap();
    assert!(lock_log.contains(&format!("args=lock --platform {platform} ")), "{lock_log}");
    let last = lock_log.rsplit("dir=").next().unwrap();
    assert!(last.contains("[tools]") && !last.contains("[env]") && !last.contains("HOOK"), "{last}");
    assert!(!last.contains("/app"), "never in the project: {last}");
    assert!(!fixture.dir.path().join("app/hook-ran").exists());
    let stack_lock = fs::read_to_string(fixture.dir.path().join("app/stack.lock")).unwrap();
    assert!(stack_lock.contains("version = 3") && stack_lock.contains("[provider_lock]") && !stack_lock.contains("aube"), "{stack_lock}");

    let before = mise_log(&fixture).len();
    let result = json_result(&fixture.ok(&["install", "--json"]));
    let calls: Vec<String> = mise_log(&fixture)[before..].lines().filter(|l| l.starts_with("install")).map(|l| l.trim().to_string()).collect();
    assert_eq!(calls, ["install --locked --yes --quiet jq rust", "install --yes --quiet npm:prettier uv"], "{result}");
    let detail = &result["data"]["steps"].as_array().unwrap().iter().find(|s| s["step"] == "install").unwrap()["detail"];
    assert_eq!(detail["locked"], json!(["jq", "rust"]));
    assert_eq!(detail["plain"], json!(["npm:prettier", "uv"]));
    assert_eq!(detail["artifacts"]["platform"], platform.as_str());
    assert_eq!(detail["artifacts"]["verified"], json!(["jq@1.7.1"]));
    assert_eq!(detail["artifacts"]["exempt"], json!(["rust@1.93.1"]));
    assert_eq!(detail["artifacts"]["unsupported"], json!(["npm:prettier@3.6.2"]));
    assert_eq!(detail["artifacts"]["missing"], json!(["uv@0.9.0"]));
    assert!(detail["boundary"].as_str().unwrap().contains("already installed"));
    // The lock mise checked against was rendered from stack.lock before the install.
    let rendered = fs::read_to_string(fixture.dir.path().join("rendered-at-install")).unwrap();
    assert!(rendered.contains("sha256:0bbe619e") && !rendered.contains("aube") && !rendered.contains("provider ="), "{rendered}");

    // status reports this platform's coverage; exec neither installs nor renders.
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["artifacts"]["verified"], json!(["jq@1.7.1"]), "{status}");
    fs::remove_file(fixture.dir.path().join("app/.config/mise/mise.lock")).unwrap();
    let before = mise_log(&fixture).len();
    fixture.ok(&["exec", "--", "true"]);
    fixture.command(&["status", "--json"]).output().unwrap();
    assert!(!fixture.dir.path().join("app/.config/mise/mise.lock").exists());
    assert!(!mise_log(&fixture)[before..].contains("install") && !mise_log(&fixture)[before..].contains("lock --platform"));
    // A second ordinary compile with nothing new to lock asks mise for nothing.
    fs::remove_file(fixture.dir.path().join("lock.log")).unwrap();
    let inspect = json_result(&fixture.ok(&["inspect", "--json"]));
    let jq = inspect["data"]["versions"].as_array().unwrap().iter().find(|v| v["name"] == "jq").unwrap().clone();
    assert_eq!(jq["backend"], "aqua:jqlang/jq");
    assert_eq!(jq["artifacts"][platform.as_str()]["state"], "verified", "{jq}");
}

#[test]
fn refused_downloads_and_signers_are_artifact_mismatch_and_other_failures_install_failed() {
    let (fixture, platform) = artifact_fixture("");
    let checksum = format!("mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on {platform} locks https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64: Checksum mismatch for file /tmp/x/jq-macos-arm64:\nExpected: sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a\nActual:   sha256:1111\nhint: GitHub's current digest for jq-macos-arm64 in jqlang/jq jq-1.7.1 matches this download (asset updated 2026-06-20T14:10:29Z, release published 2026-06-20T14:11:27Z), so the expected checksum is out of date: the maintainer likely re-uploaded the asset. If you trust the new upload, update the checksum in mise.lock.\n");
    fs::write(fixture.dir.path().join("install-locked-fail"), &checksum).unwrap();
    let before = mise_log(&fixture).len();
    let out = fixture.command(&["install", "--json"]).output().unwrap();
    let e = &json_result(&out)["error"];
    assert_eq!(e["code"], "artifact_mismatch", "{e}");
    assert_eq!(e["details"][0], json!({ "kind": "checksum", "name": "jq@1.7.1", "platform": platform, "expected": "sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a", "actual": "sha256:1111", "url": "https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64", "upstream": "GitHub's current digest for jq-macos-arm64 in jqlang/jq jq-1.7.1 matches this download (asset updated 2026-06-20T14:10:29Z, release published 2026-06-20T14:11:27Z)" }));
    assert!(e["hint"].as_str().unwrap().contains("compile --update"));
    // mise renders its lock from stack.lock: its advice to edit that lock is not passed on.
    let output = e["details"][1]["output"].as_str().unwrap();
    assert!(output.contains("Expected: sha256:0bbe") && output.contains("Actual:   sha256:1111"), "{output}");
    assert!(!output.contains("mise.lock") && !output.contains("re-uploaded"), "{output}");
    assert!(!mise_log(&fixture)[before..].contains("install --yes"), "nothing else is installed after a refusal");

    fs::write(fixture.dir.path().join("install-locked-fail"), "mise ERROR Failed to install packslip:github.com/jdx/fnox@1.39.0: mise.lock says sigstore-oidc:a signed fnox@1.39.0, but this release is signed by sigstore-oidc:b; remove the entry from mise.lock to accept the new signer\n").unwrap();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!((e["code"].as_str(), e["details"][0]["kind"].as_str(), e["details"][0]["expected"].as_str()), (Some("artifact_mismatch"), Some("signer"), Some("sigstore-oidc:a")));
    assert!(!e["details"][1]["output"].as_str().unwrap().contains("remove the entry"), "{e}");

    fs::write(fixture.dir.path().join("install-locked-fail"), "mise ERROR network unreachable\n").unwrap();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "install_failed");
}

#[test]
fn a_task_runs_against_the_lock_rendered_from_stack_lock_never_a_missing_or_stale_project_file() {
    let (fixture, _) = artifact_fixture("[tasks.q]\nrun='jq --version'\n");
    let app = fixture.dir.path().join("app");
    let expected = stack::artifacts::rendered(stack::lock::read(&app).unwrap().as_ref()).unwrap();
    assert!(expected.contains("sha256:0bbe619e"), "{expected}");
    let project_lock = app.join(".config/mise/mise.lock");
    let run_lock = fixture.dir.path().join("run-lock");
    // A fresh checkout (nothing rendered), then a lock rendered from an earlier stack.lock.
    for project in [None, Some("# stale\n[[tools.jq]]\nversion = \"1.7.1\"\n")] {
        match project {
            None => {
                let _ = fs::remove_file(&project_lock);
            }
            Some(text) => fs::write(&project_lock, text).unwrap(),
        }
        for route in ["cli", "mcp"] {
            let _ = fs::remove_file(&run_lock);
            let data = match route {
                "cli" => json_result(&fixture.command(&["--json", "run", "q"]).output().unwrap())["data"].clone(),
                _ => fixture.mcp(&[("stack_run", json!({ "task": "q" }))], &[])[0]["structuredContent"]["data"].clone(),
            };
            assert_eq!(data["exit_code"], 0, "{route}: {data}");
            assert_eq!(fs::read_to_string(&run_lock).unwrap(), expected, "{route}, project lock {project:?}");
        }
        // The project's own file is left as it was.
        assert_eq!(fs::read_to_string(&project_lock).ok().as_deref(), project);
    }
}

#[test]
fn a_tasks_missing_pins_install_from_its_copy_before_it_runs_and_a_warm_run_installs_nothing() {
    let (fixture, _) = artifact_fixture("[tasks.q]\nrun='jq --version'\n");
    let dir = fixture.dir.path();
    let _ = fs::remove_file(dir.join("app/.config/mise/mise.lock"));
    let before = mise_log(&fixture).len();
    let data = json_result(&fixture.command(&["--json", "run", "q"]).output().unwrap())["data"].clone();
    assert_eq!(data["exit_code"], 0, "{data}");
    let calls: Vec<String> = mise_log(&fixture)[before..].lines().filter(|l| l.starts_with("install") || l.starts_with("run")).map(|l| l.trim().to_string()).collect();
    // stack install's split, before the task starts: checked pins locked, the rest plain.
    assert_eq!(calls, ["install --locked --yes --quiet jq rust", "install --yes --quiet npm:prettier uv", "run --skip-deps --no-timings q --"], "{data}");
    // From the task's copy, with the lock rendered from stack.lock beside it.
    let install_log = fs::read_to_string(dir.join("install.log")).unwrap();
    assert!(install_log.lines().all(|l| l.contains("/task-config/")), "{install_log}");
    let expected = stack::artifacts::rendered(stack::lock::read(&dir.join("app")).unwrap().as_ref()).unwrap();
    assert_eq!(fs::read_to_string(dir.join("rendered-at-install")).unwrap(), expected);
    assert!(!dir.join("app/.config/mise/mise.lock").exists(), "the project's files are not written");
    // Everything installed: only the installed query, beside the environment.
    for route in ["cli", "mcp"] {
        let before = mise_log(&fixture).len();
        let data = match route {
            "cli" => json_result(&fixture.command(&["--json", "run", "q"]).output().unwrap())["data"].clone(),
            _ => fixture.mcp(&[("stack_run", json!({ "task": "q" }))], &[])[0]["structuredContent"]["data"].clone(),
        };
        assert_eq!(data["exit_code"], 0, "{route}: {data}");
        assert!(!mise_log(&fixture)[before..].lines().any(|l| l.starts_with("install") || l.starts_with("version")), "{route}");
    }
}

#[test]
fn a_cold_tasks_install_is_given_the_planned_env_its_copy_declares_and_nothing_else() {
    // mise renders the copy's `{{ env["KEY"] }}` declarations while installing, as when running.
    let (fixture, _) = artifact_fixture("[env]\nTASK_ENV='from-project'\n[tasks.q]\nrun='echo $TASK_ENV'\n");
    let dir = fixture.dir.path();
    let routes = ["cli", "mcp", "terminal"];
    for route in routes {
        let _ = fs::remove_file(dir.join("installed"));
        let _ = fs::remove_file(dir.join("install-env.log"));
        fixture.set_env(&[("TASK_ENV", "from-project".into()), ("UNDECLARED", "not-for-install".into())]);
        let ran = match route {
            "cli" => json_result(&fixture.command(&["--json", "run", "q"]).output().unwrap())["data"]["exit_code"] == 0,
            "mcp" => fixture.mcp(&[("stack_run", json!({ "task": "q" }))], &[])[0]["structuredContent"]["data"]["exit_code"] == 0,
            _ => fixture.command(&["run", "q"]).output().unwrap().status.success(),
        };
        assert!(ran, "{route}: {}", fs::read_to_string(dir.join("mise.log")).unwrap_or_default());
        // Exactly the declared names, with the values planning read; nothing it did not declare.
        let given = fs::read_to_string(dir.join("install-env.log")).unwrap();
        assert!(given.lines().all(|l| l == "TASK_ENV=from-project") && !given.is_empty(), "{route}: {given}");
    }
}

#[test]
fn a_task_whose_pins_mise_refuses_fails_with_stacks_remedy_before_it_runs() {
    let (fixture, platform) = artifact_fixture("[tasks.q]\nrun='jq --version'\n");
    let dir = fixture.dir.path();
    let refusal = format!("mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on {platform} locks https://example.invalid/jq: Checksum mismatch for file /tmp/x/jq:\nExpected: sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a\nActual:   sha256:11\nhint: GitHub's current digest for jq matches this download (asset updated t1, release published t2), so the expected checksum is out of date: the maintainer likely re-uploaded the asset. If you trust the new upload, update the checksum in mise.lock.\n");
    fs::write(dir.join("install-locked-fail"), &refusal).unwrap();
    let before = mise_log(&fixture).len();
    let cli = fixture.command(&["--json", "run", "q"]).output().unwrap();
    let mcp = fixture.mcp(&[("stack_run", json!({ "task": "q" }))], &[])[0]["structuredContent"].clone();
    for e in [json_result(&cli)["error"].clone(), mcp["error"].clone()] {
        assert_eq!(e["code"], "artifact_mismatch", "{e}");
        assert_eq!(e["details"][0]["name"], "jq@1.7.1", "{e}");
        assert!(e["hint"].as_str().unwrap().contains("stack compile --update"), "{e}");
        assert!(!e.to_string().contains("update the checksum in mise.lock"), "{e}");
    }
    // In a terminal: stack's error, never mise's advice to edit the lock.
    let terminal = fixture.command(&["run", "q"]).output().unwrap();
    assert!(!terminal.status.success());
    let text = format!("{}{}", String::from_utf8_lossy(&terminal.stdout), String::from_utf8_lossy(&terminal.stderr));
    assert!(text.contains("artifact_mismatch") || text.contains("does not match stack.lock"), "{text}");
    assert!(text.contains("stack compile --update") && !text.contains("mise.lock"), "{text}");
    assert!(!mise_log(&fixture)[before..].contains("run --skip-deps"), "the task never started");
}

#[test]
fn a_refused_cold_install_leaves_no_tracking_link_to_the_removed_task_copy() {
    let (fixture, platform) = artifact_fixture("[tasks.q]\nrun='jq --version'\n");
    let dir = fixture.dir.path();
    fs::write(dir.join("install-locked-fail"), format!("mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on {platform} locks https://example.invalid/jq: Checksum mismatch for file /tmp/x/jq:\nExpected: sha256:00\nActual:   sha256:11\n")).unwrap();
    let state = dir.join("mise-state");
    let tracked = state.join("tracked-configs");
    fs::create_dir_all(&tracked).unwrap();
    fs::write(dir.join("track-configs"), "").unwrap();
    let project_config = dir.join("app/.config/mise/conf.d/stack.toml");
    std::os::unix::fs::symlink(&project_config, tracked.join("project")).unwrap();
    for route in ["cli", "terminal"] {
        let mut command = fixture.command(&if route == "cli" { vec!["--json", "run", "q"] } else { vec!["run", "q"] });
        let out = command.env("MISE_STATE_DIR", &state).output().unwrap();
        assert!(!out.status.success(), "{route}");
        assert!(fs::read_to_string(dir.join("install.log")).unwrap().contains("/task-config/"), "{route}: the install loaded the copy");
        let left: Vec<_> = fs::read_dir(&tracked).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["project"], "{route}: the refused install's link to its copy must go with it");
    }
}

#[test]
fn a_stalled_cold_install_is_cut_short_by_the_runs_timeout_and_the_next_call_runs() {
    let (fixture, _) = artifact_fixture("[tasks.q]\nrun='jq --version'\n");
    let dir = fixture.dir.path();
    let stalled_ended = || {
        let pid: i32 = fs::read_to_string(dir.join("install-stalled-pid")).unwrap().trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // SAFETY: signal 0 only checks that the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe { libc::kill(pid, 0) != 0 }
    };
    let ran = || fs::read_to_string(dir.join("mise.log")).unwrap().lines().filter(|l| l.starts_with("run --skip-deps")).count();
    let task_configs = || fs::read_dir(dir.join("cache/task-config")).map(|d| d.count()).unwrap_or(0);
    for route in ["cli", "terminal", "mcp"] {
        let _ = fs::remove_file(dir.join("installed"));
        let _ = fs::remove_file(dir.join("install-stalled-pid"));
        fs::write(dir.join("install-stall"), "30").unwrap();
        let before = ran();
        let start = Instant::now();
        let error = match route {
            "cli" => {
                let out = fixture.command(&["--json", "run", "--timeout", "1s", "q"]).output().unwrap();
                assert_eq!(out.status.code(), Some(124), "{route}");
                json_result(&out)["error"].clone()
            }
            "terminal" => {
                let out = fixture.command(&["run", "--timeout", "1s", "q"]).output().unwrap();
                assert_eq!(out.status.code(), Some(124), "{route}: {}", String::from_utf8_lossy(&out.stderr));
                assert!(String::from_utf8_lossy(&out.stderr).contains("timed_out"), "{route}");
                json!({ "code": "timed_out", "details": [{ "cause": {} }] })
            }
            _ => {
                // The same server answers the next call once the stalled one gave up.
                let results = fixture.mcp(&[("stack_run", json!({ "task": "q", "timeout_secs": 1 })), ("stack_status", json!({}))], &[]);
                assert_eq!(results[1]["structuredContent"]["ok"], true, "{route}: {}", results[1]);
                results[0]["structuredContent"]["error"].clone()
            }
        };
        assert!(start.elapsed() < Duration::from_secs(8), "{route}: took {:?}", start.elapsed());
        assert_eq!(error["code"], "timed_out", "{route}: {error}");
        assert!(error["details"][0].get("cause").is_some(), "{route}: what was cut short: {error}");
        assert!(stalled_ended(), "{route}: the installer's group is killed");
        assert_eq!(ran(), before, "{route}: the task never started");
        assert_eq!(task_configs(), 0, "{route}: the task's copy is removed");
        // The project lock was released: a call without the stall installs and runs.
        fs::remove_file(dir.join("install-stall")).unwrap();
        let data = json_result(&fixture.command(&["--json", "run", "--timeout", "30s", "q"]).output().unwrap())["data"].clone();
        assert_eq!(data["exit_code"], 0, "{route}: {data}");
    }
}

#[test]
fn a_tasks_own_output_is_returned_as_it_wrote_it_and_never_read_for_refusals() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tasks.refused]\nrun='jq .'\n");
    let refusal = "mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on macos-arm64 locks https://example.invalid/jq: Checksum mismatch for file /tmp/x/jq:\nExpected: sha256:00\nActual:   sha256:11\nhint: update the checksum in mise.lock.\n";
    fs::write(fixture.dir.path().join("run-refusal"), refusal).unwrap();
    for data in [
        json_result(&fixture.command(&["--json", "run", "refused"]).output().unwrap())["data"].clone(),
        fixture.mcp(&[("stack_run", json!({ "task": "refused" }))], &[])[0]["structuredContent"]["data"].clone(),
    ] {
        assert_eq!(data["exit_code"], 1, "{data}");
        assert_eq!(data["stderr"], refusal, "the task's output is not rewritten");
        assert!(data.get("warnings").is_none(), "{data}");
    }
}

#[test]
fn version_2_locks_install_plainly_and_required_policies_refuse_before_any_install() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    // The lock the fixture compiled, turned into version 2.
    let v3 = fs::read_to_string(fixture.dir.path().join("app/stack.lock")).unwrap();
    let v2 = v3.replace("version = 3", "version = 2");
    let v2 = v2.split("[provider_lock]").next().unwrap().to_string();
    fs::write(fixture.dir.path().join("app/stack.lock"), &v2).unwrap();
    let before = mise_log(&fixture).len();
    let result = json_result(&fixture.ok(&["install", "--json"]));
    let log = &mise_log(&fixture)[before..];
    assert!(log.contains("install --yes --quiet jq") && !log.contains("--locked"), "{log}");
    let detail = &result["data"]["steps"].as_array().unwrap().iter().find(|s| s["step"] == "install").unwrap()["detail"];
    assert_eq!(detail["artifacts"]["missing"], json!(["jq@1.7.1"]));
    assert!(!fixture.dir.path().join("app/.config/mise/mise.lock").exists(), "nothing to render from version 2");
    assert_eq!(fs::read_to_string(fixture.dir.path().join("app/stack.lock")).unwrap(), v2, "install never rewrites it");

    let required = |extra: &str| fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n[lock]\nartifacts='required'\n{extra}")).unwrap();
    required("");
    let before = mise_log(&fixture).len();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "lock_outdated", "{e}");
    assert!(!mise_log(&fixture)[before..].contains("install"));

    // A version 3 lock that leaves a pin unsupported, or this platform unlisted.
    let (fixture, _) = artifact_fixture("");
    fs::write(fixture.dir.path().join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[lock]\nplatforms=['current']\nartifacts='required'\n").unwrap();
    let before = mise_log(&fixture).len();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "artifact_unlocked", "{e}");
    assert!(e["details"].as_array().unwrap().iter().any(|d| d["name"] == "npm:prettier@3.6.2" && d["state"] == "unsupported"), "{e}");
    assert!(!mise_log(&fixture)[before..].contains("install"));

    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    let other = if stack::artifacts::current_platform() == "linux-x64" { "linux-arm64" } else { "linux-x64" };
    fs::write(fixture.dir.path().join("mise-lock.toml"), tool_lock(other).split("[[tools.rust]]").next().unwrap()).unwrap();
    fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n[lock]\nplatforms=['{other}']\nartifacts='required'\n")).unwrap();
    fixture.ok(&["compile"]);
    let before = mise_log(&fixture).len();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "artifact_unlocked", "{e}");
    assert_eq!(e["details"][0]["state"], "unlisted");
    assert!(!mise_log(&fixture)[before..].contains("install"));
}

/// stack.lock, the generated config, the rendered lock and the machine state, byte for byte.
fn snapshot(fixture: &Fixture) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
    let root = fixture.dir.path();
    let mut files: std::collections::BTreeMap<String, Option<Vec<u8>>> = ["app/stack.lock", "app/.config/mise/conf.d/stack.toml", "app/.config/mise/mise.lock", "app/.stack/session.json"]
        .iter()
        .map(|f| (f.to_string(), fs::read(root.join(f)).ok()))
        .collect();
    for dir in ["state", "state/sessions"] {
        for entry in fs::read_dir(root.join(dir)).into_iter().flatten().flatten().filter(|e| e.path().is_file()) {
            let name = entry.path().strip_prefix(root).unwrap().display().to_string();
            files.insert(name, fs::read(entry.path()).ok());
        }
    }
    files
}

#[test]
fn every_locked_operation_refuses_an_unlisted_platform_under_required_and_ordinary_compile_does_not() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n[tasks.lint]\nrun='true'\n");
    let other = if stack::artifacts::current_platform() == "linux-x64" { "linux-arm64" } else { "linux-x64" };
    fs::write(fixture.dir.path().join("mise-lock.toml"), tool_lock(other).split("[[tools.rust]]").next().unwrap()).unwrap();
    fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n[lock]\nplatforms=['{other}']\nartifacts='required'\n")).unwrap();
    // Locking for another platform is ordinary compile's job, from any machine.
    let compiled = json_result(&fixture.ok(&["compile", "--json"]));
    let jq = compiled["data"]["versions"].as_array().unwrap().iter().find(|v| v["name"] == "jq").unwrap().clone();
    assert_eq!(jq["artifacts"][other]["state"], "verified", "{jq}");

    let before = snapshot(&fixture);
    let calls = mise_log(&fixture).len();
    let refused = |e: &Value, context: &str| {
        assert_eq!(e["code"], "artifact_unlocked", "{context}: {e}");
        let detail = e["details"].as_array().unwrap().iter().find(|d| d["state"] == "unlisted").unwrap_or_else(|| panic!("{context}: {e}"));
        assert_eq!(detail["platform"], stack::artifacts::current_platform().as_str(), "{context}");
        assert_eq!(detail["platforms"], json!([other]), "{context}");
    };
    for args in [
        &["--json", "install"][..],
        &["--json", "up"],
        &["--json", "exec", "--", "touch", "ran"],
        &["--json", "run", "lint"],
        &["--json", "status"],
        &["--json", "inspect"],
        &["--json", "compile", "--locked"],
    ] {
        let out = fixture.command(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        refused(&json_result(&out)["error"], &format!("{args:?}"));
    }
    let results = fixture.mcp(
        &[
            ("stack_install", json!({})),
            ("stack_up", json!({})),
            ("stack_exec", json!({ "command": ["touch", "ran"] })),
            ("stack_run", json!({ "task": "lint" })),
            ("stack_status", json!({})),
            ("stack_inspect", json!({})),
            ("stack_compile", json!({ "locked": true })),
        ],
        &[],
    );
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result["isError"], true, "{i}: {result}");
        refused(&result["structuredContent"]["error"], &format!("mcp call {i}"));
    }
    assert!(!fixture.dir.path().join("app/ran").exists(), "the command never ran");
    let log = &mise_log(&fixture)[calls..];
    assert!(log.is_empty(), "no provider call before the refusal:\n{log}");
    assert_eq!(snapshot(&fixture), before, "nothing was written");
}

#[test]
fn a_mise_too_old_for_the_lock_fails_install_and_up_before_ports_identities_or_config_change() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n[services.web]\nrun='true'\n");
    let platform = stack::artifacts::current_platform();
    fs::write(fixture.dir.path().join("mise-lock.toml"), tool_lock(&platform).split("[[tools.rust]]").next().unwrap()).unwrap();
    fs::write(fixture.dir.path().join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[lock]\nplatforms=['current']\n").unwrap();
    fixture.ok(&["compile"]);
    // A fresh machine with a committed v3 lock: no reservations, an earlier generated config.
    for f in ["state/ports.json", "state/identities.json", "app/.config/mise/mise.lock"] {
        let _ = fs::remove_file(fixture.dir.path().join(f));
    }
    let config = fixture.dir.path().join("app/.config/mise/conf.d/stack.toml");
    fs::write(&config, "# an earlier generated config must survive the failure\n").unwrap();
    fs::write(fixture.dir.path().join("mise-version"), "2026.9.15 macos-arm64 (2026-09-15)\n").unwrap();
    let before = snapshot(&fixture);
    assert!(before.keys().all(|f| !f.ends_with("ports.json")), "{:?}", before.keys());

    let check = |e: &Value, context: &str| {
        assert_eq!(e["code"], "provider_outdated", "{context}: {e}");
        assert!(e["message"].as_str().unwrap().contains("2026.9.16"), "{context}: {e}");
        let progress = e["details"].as_array().unwrap().iter().find(|d| d.get("steps").is_some()).unwrap_or_else(|| panic!("{context}: {e}"));
        assert_eq!(progress["changed"], false, "{context}");
        let last = progress["steps"].as_array().unwrap().last().unwrap().clone();
        assert_eq!((last["step"].as_str(), last["status"].as_str()), (Some("install"), Some("failed")), "{context}: {e}");
        assert!(progress["steps"].as_array().unwrap().iter().all(|s| s["step"] != "compile"), "compile never completed: {e}");
    };
    let calls = mise_log(&fixture).len();
    for args in [&["--json", "install"][..], &["--json", "up"]] {
        let out = fixture.command(args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        check(&json_result(&out)["error"], &format!("{args:?}"));
    }
    let results = fixture.mcp(&[("stack_install", json!({})), ("stack_up", json!({}))], &[]);
    for (i, result) in results.iter().enumerate() {
        check(&result["structuredContent"]["error"], &format!("mcp call {i}"));
    }
    let log = &mise_log(&fixture)[calls..];
    assert_eq!(log.lines().collect::<Vec<_>>(), ["version"; 4], "one release check per command and nothing else");
    assert_eq!(snapshot(&fixture), before, "no reservation, generated config, rendered lock or session");

    // The release the lock needs: the same install now publishes and installs.
    fs::write(fixture.dir.path().join("mise-version"), "2026.9.16 macos-arm64 (2026-09-16)\n").unwrap();
    fixture.ok(&["install"]);
    assert!(fixture.dir.path().join("state/ports.json").exists());
    assert!(!fs::read_to_string(&config).unwrap().contains("earlier"));
    assert!(fixture.dir.path().join("app/.config/mise/mise.lock").exists());
}

#[test]
fn a_mise_too_old_for_the_lock_stops_install_before_rendering_and_doctor_says_so() {
    let (fixture, _) = artifact_fixture("");
    fs::remove_file(fixture.dir.path().join("app/.config/mise/mise.lock")).unwrap();
    fs::write(fixture.dir.path().join("mise-version"), "2026.9.15 macos-arm64 (2026-09-15)\n").unwrap();
    let before = mise_log(&fixture).len();
    let e = json_result(&fixture.command(&["install", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "provider_outdated", "{e}");
    assert!(e["message"].as_str().unwrap().contains("2026.9.16"));
    assert!(!fixture.dir.path().join("app/.config/mise/mise.lock").exists());
    assert!(!mise_log(&fixture)[before..].contains("install"));
    let doctor = json_result(&fixture.command(&["doctor", "--json"]).output().unwrap());
    let check = doctor["error"]["details"].as_array().unwrap().iter().find(|c| c["name"] == "mise_release").unwrap().clone();
    assert_eq!(check["ok"], false, "{doctor}");
    assert!(check["detail"].as_str().unwrap().contains("2026.9.16"), "{check}");
    let e = json_result(&fixture.command(&["compile", "--locked", "--json"]).output().unwrap())["error"].clone();
    assert_eq!(e["code"], "provider_outdated");
}
