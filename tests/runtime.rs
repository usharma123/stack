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
#[path = "support/servers.rs"]
mod servers;

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
        // `STACK_TEST_BIN` runs another build (to show a regression test fails without its fix).
        let mut command = Command::new(std::env::var("STACK_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_stack").to_string()));
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
            .env("STACK_CACHE_DIR", self.dir.path().join("cache"))
            // Where mise's presets kept data for an older Stack (see `legacy_data`).
            .env("MISE_STATE_DIR", self.dir.path().join("mise-state"))
            // The supervisor's state directory is the test's own unless a test names one: Stack
            // writes its supervisor lock there, never in the caller's Pitchfork state.
            .env("PITCHFORK_STATE_DIR", self.dir.path().join("pfs"));
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

    /// The project's session record, with what `up` and `restart` leave out of their results.
    fn session(&self) -> Value {
        serde_json::from_slice(&fs::read(self.dir.path().join("app/.stack/session.json")).unwrap()).unwrap()
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
fn a_comma_list_given_to_require_is_answered_with_one_flag_per_service() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n[services.api]\nrun='true'\n");
    let out = fixture.command(&["--json", "exec", "--require", "web,api", "--", "true"]).output().unwrap();
    let error = json_result(&out)["error"].clone();
    assert_eq!(error["code"], "unknown_service", "{error}");
    assert_eq!(error["hint"], "name one service per flag: --require web --require api (services: web, api)", "{error}");
}

#[test]
fn init_writes_a_project_that_compiles_and_never_replaces_one() {
    let fixture = Fixture::new();
    let fresh = fixture.dir.path().join("fresh");
    fs::create_dir(&fresh).unwrap();
    let out = fixture.command_at(&fresh, &["--json", "init", "--tool", "jq=1.7.1", "--service", "db=postgres@17"]).output().unwrap();
    let result = json_result(&out);
    assert!(out.status.success(), "{result}");
    assert_eq!((result["data"]["tools"].clone(), result["data"]["services"].clone()), (json!(["jq"]), json!(["db"])), "{result}");
    let compiled = fixture.command_at(&fresh, &["--json", "compile"]).output().unwrap();
    assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stdout));
    let again = fixture.command_at(&fresh, &["--json", "init"]).output().unwrap();
    assert_eq!(json_result(&again)["error"]["code"], "project_exists");
    let bad = fixture.command_at(&fresh, &["--json", "init", "--service", "web=nginx"]).output().unwrap();
    assert_eq!(json_result(&bad)["error"]["code"], "project_exists", "an existing file is checked first");
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
fn stacks_own_directory_stays_ignored_after_up_and_through_the_old_exclude_migration() {
    let fixture = Fixture::new();
    let top = fixture.dir.path();
    git(top, &["init", "-q"]);
    fixture.ok(&["up"]);
    let stack_warning = |out: &Output| json_result(out)["data"].get("warnings").filter(|w| w.to_string().contains(".stack")).cloned();
    assert_eq!(stack_warning(&fixture.ok(&["--json", "compile"])), None);
    assert_eq!(untracked(top, "app/.stack"), Vec::<String>::new());
    // A session from a release that hid .stack with a block in the shared exclude instead.
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    fs::remove_file(app.join(".stack/.gitignore")).unwrap();
    let exclude = top.join(".git/info/exclude");
    fs::write(&exclude, format!("# mine\n\n# stack: generated files of {a}\n/app/.stack/\n# stack: end of {a}\n", a = app.display())).unwrap();
    assert_eq!(untracked(top, "app/.stack"), Vec::<String>::new());
    assert_eq!(stack_warning(&fixture.ok(&["--json", "compile"])), None);
    assert_eq!(fs::read_to_string(&exclude).unwrap(), "# mine\n");
    assert!(app.join(".stack/.gitignore").is_file());
    assert_eq!(untracked(top, "app/.stack"), Vec::<String>::new(), "the session shows in git");
    fixture.ok(&["down"]);
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
    // A recorded process Stack's supervisor does not run (and so cannot stop) is still alive:
    // `down` must not confirm, and keeps the record.
    session["services"] = json!({ "worker": { "port":0, "pid":std::process::id(), "identity":"liveness", "verified_at":0 } });
    for path in &paths {
        fs::write(path, session.to_string()).unwrap();
    }
    let out = fixture.command(&["down", "--json"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"], "stop_unconfirmed");
    for path in paths {
        assert_eq!(serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(), session);
    }
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
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_secs(5));
    let mut server = Server::start(&fixture);
    let started = Instant::now();
    let result = server.call("stack_exec", json!({ "command": ["sleep", "30"], "timeout_secs": 1 }));
    let elapsed = started.elapsed();
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    assert!(elapsed < Duration::from_secs(4), "the call stays bounded: {elapsed:?}");
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

#[test]
fn a_command_without_a_timeout_does_not_wait_unbounded_to_finish_its_record() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_secs(8));
    let started = Instant::now();
    let out = fixture.command(&["--json", "exec", "--", "sleep", "1"]).output().unwrap();
    let elapsed = started.elapsed();
    assert_eq!(json_result(&out)["ok"], true, "{}", String::from_utf8_lossy(&out.stdout));
    assert!(elapsed < Duration::from_secs(5), "waited for the lock: {elapsed:?}");
    assert_eq!(fs::read_dir(completions(&fixture)).unwrap().count(), 1);
    holder.join().unwrap();
    fixture.ok(&["up"]);
    assert_eq!(read_value(&session_file)["active_executions"], json!({}));
    assert_eq!(fs::read_dir(completions(&fixture)).unwrap().count(), 0);
}

#[test]
fn an_expired_lease_is_not_healthy_until_it_is_renewed() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "1s"]);
    thread::sleep(Duration::from_millis(2200));
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status = json_result(&out);
    assert_eq!(out.status.code(), Some(1), "{status}");
    assert_eq!(status["data"]["healthy"], false, "{status}");
    let why = status["data"]["lease_expired"].as_str().unwrap();
    assert!(why.starts_with("lease expired") && why.contains("the next `stack gc` or `stack up` stops") && why.contains("stack renew"), "{why}");
    fixture.ok(&["renew"]);
    let status = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!((status["data"]["healthy"].clone(), status["data"].get("lease_expired")), (json!(true), None), "{status}");
}

#[test]
fn env_renews_the_lease_as_status_does_and_registers_nothing_as_running() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "30s"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let session_file = app.join(".stack/session.json");
    let before = read_value(&session_file);
    let renewed = before["lease"]["renewed_at"].as_u64().unwrap();
    while unix_now() <= renewed {
        thread::sleep(Duration::from_millis(20));
    }
    let out = fixture.ok(&["env"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("STACK_SESSION='{}'", before["id"].as_str().unwrap())));
    let after = read_value(&session_file);
    assert!(after["lease"]["renewed_at"].as_u64().unwrap() > renewed, "{after}");
    assert_eq!(after["active_executions"], json!({}));
    assert!(fs::read_dir(completions(&fixture)).map_or(true, |d| d.count() == 0));
    assert_eq!(after["id"], before["id"]);
    fixture.ok(&["down"]);
}

#[test]
fn a_renewal_made_while_a_finished_command_waits_for_the_lock_is_kept() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "1s"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    let exec = fixture
        .command(&["--json", "exec", "--", "sh", "-c", "while test ! -e finish; do sleep 0.01; done"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_value(&session_file)["active_executions"].as_object().map_or(true, |e| e.is_empty()) {
        assert!(Instant::now() < deadline, "execution never registered");
        thread::sleep(Duration::from_millis(5));
    }
    let renew = {
        let _lock = stack::state::project_lock(&state, &app).unwrap();
        // The command finishes late in one second; `renew` waits for the lock with its
        // release, and gets it first, in the next second.
        while !(700..740).contains(&std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_millis()) {
            thread::sleep(Duration::from_millis(2));
        }
        fs::write(app.join("finish"), "").unwrap();
        let renew = fixture.command(&["--json", "renew"]).stdout(Stdio::piped()).spawn().unwrap();
        thread::sleep(Duration::from_millis(480));
        renew
    };
    let renewed = json_result(&renew.wait_with_output().unwrap())["data"]["lease"]["renewed_at"].as_u64().unwrap();
    let out = exec.wait_with_output().unwrap();
    assert_eq!(json_result(&out)["ok"], true, "{}", String::from_utf8_lossy(&out.stdout));
    let lease = read_value(&session_file)["lease"].clone();
    assert!(lease["renewed_at"].as_u64().unwrap() >= renewed, "renewed at {renewed}, then moved back: {lease}");
    // One second after that renewal the session is not yet idle past its TTL.
    while unix_now() < renewed + 1 {
        thread::sleep(Duration::from_millis(5));
    }
    let out = fixture.ok(&["gc", "--json"]);
    assert_eq!(json_result(&out)["data"], json!([]), "reclaimed one second after a renewal");
    fixture.ok(&["down"]);
}

#[test]
fn a_finished_command_that_cannot_be_recorded_says_so_and_keeps_the_session_busy() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    // No completion record can be written: where they go is a file.
    fs::write(state.join("completed"), "").unwrap();
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_secs(3));
    let mut server = Server::start(&fixture);
    let result = server.call("stack_exec", json!({ "command": ["sh", "-c", "sleep 0.5; exit 3"] }));
    holder.join().unwrap();
    // The command's own result stands, with the warning beside it.
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["exit_code"], 3, "{result}");
    let warning = result["data"]["warnings"][0].as_str().unwrap_or_else(|| panic!("{result}"));
    assert!(warning.contains("still recorded as running") && warning.contains("stack down"), "{warning}");
    // Nothing claims it finished: the session stays busy while the server lives.
    assert_eq!(read_value(&session_file)["active_executions"].as_object().unwrap().len(), 1);
    let result = server.call("stack_up", json!({}));
    assert_eq!(result["error"]["code"], "session_busy", "{result}");
    server.close();
    // The same through `exec --json`, whose command's exit code is kept.
    let mut session = read_value(&session_file);
    session["active_executions"] = json!({});
    for path in fixture.session_paths() {
        fs::write(&path, session.to_string()).unwrap();
    }
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_secs(3));
    let out = fixture.command(&["--json", "exec", "--", "sh", "-c", "sleep 0.5; exit 3"]).output().unwrap();
    holder.join().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let result = json_result(&out);
    assert!(result["data"]["warnings"][0].as_str().is_some_and(|w| w.contains("still recorded as running")), "{result}");
    fixture.ok(&["down"]);
}

#[test]
fn a_command_that_cannot_start_and_cannot_be_recorded_says_so_with_its_error() {
    let fixture = Fixture::new();
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let index = fixture.session_paths().pop().unwrap();
    fs::write(state.join("completed"), "").unwrap();
    let missing = "/nonexistent/stack-review-command";
    // The command fails to start at once, so the lock is taken the moment it is registered.
    let hold = || {
        let (state, app, index) = (state.clone(), app.clone(), index.clone());
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while read_value(&index)["active_executions"].as_object().map_or(true, |e| e.is_empty()) {
                assert!(Instant::now() < deadline, "execution never registered");
            }
            let _lock = stack::state::project_lock(&state, &app).unwrap();
            thread::sleep(Duration::from_secs(3));
        })
    };
    // Should the release take the lock first all the same, it is recorded at once, so the case
    // is tried again: a release that failed leaves its execution recorded.
    let contended = |run: &mut dyn FnMut() -> Value| -> Value {
        for _ in 0..5 {
            let holder = hold();
            let error = run();
            holder.join().unwrap();
            if !read_value(&index)["active_executions"].as_object().unwrap().is_empty() || !error["details"].is_null() {
                return error;
            }
        }
        panic!("the release always took the lock first");
    };
    let check = |route: &str, error: &Value| {
        assert_eq!(error["code"], "exec_failed", "{route}: {error}");
        assert!(error["message"].as_str().unwrap().contains(missing), "{route}: {error}");
        let warning = error["details"][0]["warnings"][0].as_str().unwrap_or_else(|| panic!("{route}: {error}"));
        assert!(warning.contains("still recorded as running") && warning.contains("stack down"), "{route}: {warning}");
        assert_eq!(read_value(&index)["active_executions"].as_object().unwrap().len(), 1, "{route}");
    };
    let mut server = Server::start(&fixture);
    let error = contended(&mut || server.call("stack_exec", json!({ "command": [missing] }))["error"].clone());
    check("mcp", &error);
    let result = server.call("stack_up", json!({}));
    assert_eq!(result["error"]["code"], "session_busy", "{result}");
    server.close();
    // The same through `exec --json`.
    let mut session = read_value(&index);
    session["active_executions"] = json!({});
    for path in fixture.session_paths() {
        fs::write(&path, session.to_string()).unwrap();
    }
    let error = contended(&mut || {
        let out = fixture.command(&["--json", "exec", "--", missing]).output().unwrap();
        assert!(!out.status.success());
        json_result(&out)["error"].clone()
    });
    check("cli", &error);
    fixture.ok(&["down"]);
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
    let error = json_result(&out)["error"].clone();
    assert_eq!(error["code"], "session_busy", "{}", String::from_utf8_lossy(&out.stdout));
    // It says which stack process runs a command, and what to do.
    assert!(error["hint"].as_str().unwrap().contains(&format!("pid {}", running.id())), "{error}");
    assert!(error["hint"].as_str().unwrap().contains("stack down"), "{error}");
    assert_eq!(error["details"][0]["pid"], running.id(), "{error}");
    assert!(error["details"][0]["command"].as_str().unwrap().contains("exec"), "{error}");
    let restart = json_result(&fixture.command(&["--json", "restart"]).output().unwrap())["error"].clone();
    assert_eq!((restart["code"].clone(), restart["details"][0]["pid"].clone()), (json!("session_busy"), json!(running.id())), "{restart}");
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
fn a_completion_left_by_a_command_that_has_exited_still_renews_the_lease() {
    let fixture = Fixture::new();
    fixture.ok(&["up", "--ttl", "4s"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let state = fixture.dir.path().join("state");
    let session_file = app.join(".stack/session.json");
    let index = fixture.session_paths().pop().unwrap();
    // The command finishes while another command holds the lock, so its release leaves a
    // record; then the CLI process exits before anything applies it.
    let holder = hold_project_lock_once_executing(&state, &app, &session_file, Duration::from_millis(5500));
    let exec = fixture.command(&["--json", "exec", "--", "sleep", "3"]).stdout(Stdio::piped()).spawn().unwrap();
    let pid = exec.id();
    let out = exec.wait_with_output().unwrap();
    assert_eq!(json_result(&out)["ok"], true, "{}", String::from_utf8_lossy(&out.stdout));
    let session = read_value(&session_file);
    let (token, recorded) = session["active_executions"].as_object().unwrap().iter().next().unwrap_or_else(|| panic!("{session}"));
    assert_eq!(*recorded, json!(pid), "recorded for the coordinator, which has exited");
    let record = completions(&fixture).join(format!("{token}.json"));
    let completed_at = read_value(&record)["completed_at"].as_u64().unwrap();
    assert!(completed_at >= session["lease"]["renewed_at"].as_u64().unwrap() + 3, "{session}");
    holder.join().unwrap();
    // Idle more than the TTL since the command started, but not since it finished.
    let out = fixture.ok(&["gc", "--json"]);
    assert_eq!(json_result(&out)["data"], json!([]), "the lease counts from when the command finished");
    let saved = read_value(&index);
    assert_eq!(saved["active_executions"], json!({}));
    assert_eq!(saved["lease"]["renewed_at"], json!(completed_at));
    assert_eq!(read_value(&session_file)["lease"]["renewed_at"], json!(completed_at));
    assert!(!record.exists(), "removed once the renewal is saved");
    fixture.ok(&["down"]);
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
    fixture.sleepers();

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
        ("stack_exec", json!({ "command": ran, "timeout": "1s", "timeout_secs": 1 }), "`timeout` and `timeout_secs` cannot be given together", ""),
        ("stack_exec", json!({ "command": ran, "timeout_sec": 1 }), "unknown argument `timeout_sec`", "did you mean `timeout_secs`?"),
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
        ("stack_run", json!({ "task": "t", "secrets": ["A"] }), "unknown argument `secrets`", "secrets the task declares"),
        ("stack_run", json!({ "task": "t", "args": "a b" }), "`args` must be an array", ""),
        ("stack_run", json!({ "task": "t", "args": ["a", false] }), "`args` item false", ""),
        ("stack_run", json!({ "task": 5 }), "`task` must be a string", ""),
        ("stack_run", json!({ "task": "t", "timeout": 5 }), "`timeout` must be a string", ""),
        ("stack_compile", json!({ "update": true, "locked": true }), "`update` and `locked` cannot be given together", ""),
        ("stack_compile", json!({ "update": "yes" }), "`update` must be true or false", ""),
        ("stack_up", json!({ "ttl": 30 }), "`ttl` must be a string", ""),
        ("stack_up", json!({ "owner": 1 }), "unknown argument `owner`", "did you mean `owner_pid`?"),
        ("stack_restart", json!({ "services": "web" }), "`services` must be an array", ""),
        ("stack_logs", json!({ "service": "web", "tail": 0 }), "`tail` must be from 1 to 10000", ""),
        ("stack_logs", json!({ "grep": 5 }), "`grep` must be a string", ""),
        ("stack_inspect", json!({ "all_skills": 1 }), "`all_skills` must be true or false", ""),
        ("stack_skill", json!({ "tool": "jq", "name": 3 }), "`name` must be a string", ""),
        ("stack_status", json!({ "verbose": true }), "unknown argument `verbose`", "stack_status accepts: project, labels, all, dir"),
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
    // A malformed secret name is `invalid_secret`, as `--secret` reports it on the CLI, still
    // before anything runs.
    let malformed = fixture.mcp(&[("stack_exec", json!({ "command": ran, "secrets": ["deploy_key"] }))], &[]);
    let error = &malformed[0]["structuredContent"]["error"];
    assert_eq!(error["code"], "invalid_secret", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("upper-case variable names"), "{error}");
    assert_eq!(error["details"][0]["key"], "deploy_key", "{error}");
    let cli = json_result(&fixture.command(&["--json", "exec", "--secret", "deploy_key", "--", "true"]).output().unwrap());
    assert_eq!((cli["error"]["code"].clone(), cli["error"]["hint"].clone()), (error["code"].clone(), error["hint"].clone()), "{cli}");
    let _ = fs::remove_file(root.join("mise.log"));
    let _ = fixture.mcp(&[("stack_exec", json!({ "command": ran, "secrets": ["deploy_key"] }))], &[]);
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
    assert!(result["data"].get("signal").is_none(), "only a command a signal ended names one: {result}");

    // A command a signal ended: no exit code, the signal named, and 128 + it, like a shell.
    let out = fixture.command(&["--json", "exec", "--", "sh", "-c", "kill -KILL $$"]).output().unwrap();
    assert_eq!(out.status.code(), Some(137));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((result["ok"].clone(), result["data"]["exit_code"].clone(), result["data"]["signal"].clone()), (json!(true), Value::Null, json!(9)), "{result}");
    let out = fixture.command(&["exec", "--", "sh", "-c", "kill -KILL $$"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(137), "the terminal path reports the signal the same way");

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
fn restart_changed_restarts_exactly_the_services_whose_watched_files_changed() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nwatch=['main.py']\n");
    fs::write(fixture.dir.path().join("app/main.py"), "v1").unwrap();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let before = up["data"]["checks"][0]["pid"].clone();
    // Nothing changed: nothing restarts, and the stack is verified as it runs.
    let none = json_result(&fixture.ok(&["restart", "--changed", "--json"]));
    assert_eq!((none["data"]["restarted"].clone(), none["data"]["checks"][0]["pid"].clone()), (json!([]), before.clone()), "{none}");
    thread::sleep(Duration::from_millis(1100));
    fs::write(fixture.dir.path().join("app/main.py"), "v2").unwrap();
    let changed = json_result(&fixture.ok(&["restart", "--changed", "--json"]));
    assert_eq!(changed["data"]["restarted"], json!(["web"]), "{changed}");
    assert_ne!(changed["data"]["checks"][0]["pid"], before, "{changed}");
    assert!(changed["data"]["steps"].as_array().unwrap().iter().any(|s| s["step"] == "changed" && s["detail"] == json!(["web"])), "{changed}");
    let mcp = fixture.mcp(&[("stack_restart", json!({ "changed": true })), ("stack_restart", json!({ "changed": true, "services": ["web"] }))], &[]);
    assert_eq!(mcp[0]["structuredContent"]["data"]["restarted"], json!([]), "{}", mcp[0]);
    assert_eq!(mcp[1]["structuredContent"]["error"]["code"], "usage", "{}", mcp[1]);
    let out = fixture.command(&["restart", "--changed", "web", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    fixture.ok(&["down"]);
}

#[test]
fn restart_replaces_only_the_named_service_and_keeps_the_session() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nwatch=['main.py']\n");
    fs::write(fixture.dir.path().join("app/main.py"), "v1").unwrap();
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let before = up["data"]["checks"][0]["pid"].as_u64().unwrap();
    assert!(fixture.session()["services"]["web"]["started_at_ms"].is_u64(), "{up}");

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

    // The new process is the verified one, and it loaded the edit.
    let status = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!(status["data"]["healthy"], true, "{status}");
    assert!(status["data"]["checks"][0].get("changed_since_start").is_none(), "{status}");
    fixture.ok(&["exec", "--require", "web", "--", "true"]);

    // Logs since the start keep only what the new process wrote.
    let log = fs::read_dir(fixture.dir.path().join("state/supervisors")).unwrap().next().unwrap().unwrap().path().join("logs/web.log");
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::io::Write::write_all(&mut file, b"1000 from the old process\n").unwrap();
    std::io::Write::write_all(&mut file, format!("{} new\n", u64::MAX / 2).as_bytes()).unwrap();
    let logs = json_result(&fixture.ok(&["logs", "web", "--since-start", "--json"]));
    assert_eq!(logs["data"]["since_start"], true);
    assert_eq!(logs["data"]["lines"], json!(["new"]), "{logs}");

    let out = fixture.command(&["restart", "nope", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "unknown_service");
    fixture.ok(&["down"]);
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "no_session");
}

#[test]
fn up_and_restart_results_carry_one_check_per_service_and_leave_internals_to_the_record() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    for args in [&["up", "--ttl", "30m", "--json"][..], &["restart", "--json"]] {
        let result = json_result(&fixture.ok(args));
        let data = &result["data"];
        let session = data["session"].as_object().unwrap();
        assert_eq!(session.keys().collect::<Vec<_>>(), ["id", "started_at", "lease"], "{result}");
        assert_eq!(data["checks"][0]["service"], "web");
        assert_eq!(data["checks"][0]["port"], json!(assigned_port(&fixture, "web")));
        assert!(data["steps"].as_array().unwrap().iter().all(|s| s.get("detail").is_none()), "{result}");
        assert!(data.get("reaped").is_none() && data.get("warnings").is_none(), "{result}");
        assert!(result.to_string().len() < 800, "{} bytes: {result}", result.to_string().len());
    }
    // The record keeps everything stack itself needs.
    let record = fixture.session();
    assert!(record["config_digest"].is_string() && record["services"]["web"]["pid"].is_u64(), "{record}");
    fixture.ok(&["down"]);
}

#[test]
fn a_failed_restart_records_the_replacement_it_launched() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let old = up["data"]["checks"][0]["pid"].as_u64().unwrap();
    // The replacement starts but never becomes ready within the restart's time.
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    let out = fixture.command(&["restart", "web", "--timeout", "2s", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let replacement = fixture.session()["services"]["web"]["pid"].as_u64().expect("the replacement is recorded") as u32;
    assert_ne!(u64::from(replacement), old);
    assert!(pid_alive(replacement));
    for path in fixture.session_paths() {
        let record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["launching"], true, "{record}");
        assert_eq!(record["services"]["web"]["pid"], replacement, "{record}");
    }
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["healthy"], false, "{status}");
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "session_stale");

    // A deleted checkout's GC stops what Stack's supervisor recorded for it (by pid and start
    // time) before releasing the record.
    let moved = fixture.dir.path().join("moved");
    fs::rename(fixture.dir.path().join("app"), &moved).unwrap();
    let gc = json_result(&fixture.command_at(fixture.dir.path(), &["gc", "--json"]).output().unwrap());
    assert_eq!(gc["ok"], true, "{gc}");
    assert_eq!(fixture.index_files(), 0);
    assert!(!pid_alive(replacement));
    fs::rename(&moved, fixture.dir.path().join("app")).unwrap();
    fs::remove_file(fixture.dir.path().join("slow-start")).unwrap();
    fixture.ok(&["up"]);
    fixture.ok(&["exec", "--require", "web", "--", "true"]);
    fixture.ok(&["down"]);
}

#[test]
fn retrying_an_incomplete_launch_keeps_what_it_recorded_until_down_confirms_it() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    // A restart whose replacement does not become ready in time (see
    // `a_failed_restart_records_the_replacement_it_launched`).
    fs::write(fixture.dir.path().join("slow-start"), "30").unwrap();
    assert!(!fixture.command(&["restart", "web", "--timeout", "2s", "--json"]).output().unwrap().status.success());
    let replacement = fixture.session()["services"]["web"]["pid"].as_u64().unwrap() as u32;

    // The recovery `up` runs out of time too: the same process is still the one recorded.
    let retry = json_result(&fixture.command(&["up", "--timeout", "2s", "--json"]).output().unwrap());
    assert_eq!(retry["ok"], false, "{retry}");
    let record: Value = serde_json::from_slice(&fs::read(&fixture.session_paths()[0]).unwrap()).unwrap();
    assert_eq!(record["services"]["web"]["pid"], replacement, "{record}");
    assert_eq!(record["launching"], true, "{record}");

    // The recorded process is stopped, not abandoned.
    let down = json_result(&fixture.command(&["down", "--json"]).output().unwrap());
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(replacement), "down confirmed while {replacement} lived: {down}");
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
    fixture.ok(&["up"]);
    let start = fixture.session()["services"]["web"]["started_at_ms"].as_u64().unwrap();
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
    let pid = up["data"]["checks"][0]["pid"].clone();
    for path in fixture.session_paths() {
        let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["services"]["web"].as_object_mut().unwrap().remove("started_at_ms");
        fs::write(path, record.to_string()).unwrap();
    }
    let again = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(again["data"]["checks"][0]["pid"], pid);
    assert_eq!(fixture.session()["services"]["web"].get("started_at_ms"), None, "{again}");
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

/// A command that records its own PID and a background child's, then waits; `ignore` makes
/// both ignore SIGTERM.
fn tree_script(dir: &Path, ignore: bool) -> String {
    let trap = if ignore { "trap '' TERM; " } else { "" };
    format!("{trap}sleep 60 & echo \"$$ $!\" > {}; wait", dir.join("tree").display())
}

/// The command's and its child's PIDs, once the command wrote them.
fn started_tree(dir: &Path) -> (u32, u32) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = fs::read_to_string(dir.join("tree")) {
            let pids: Vec<u32> = text.split_whitespace().filter_map(|p| p.parse().ok()).collect();
            if let [command, child] = pids[..] {
                return (command, child);
            }
        }
        assert!(Instant::now() < deadline, "command never started");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Whether `pid` is gone within two seconds (an orphan is reaped by its new parent).
fn gone_soon(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

/// SIGTERM, then a second one after `again` if given, to a stack process; how it ended.
fn terminate(stack: &mut std::process::Child, again: Option<Duration>) -> (std::process::ExitStatus, Duration) {
    let start = Instant::now();
    unsafe { libc::kill(stack.id() as i32, libc::SIGTERM) };
    if let Some(again) = again {
        thread::sleep(again);
        unsafe { libc::kill(stack.id() as i32, libc::SIGTERM) };
    }
    (stack.wait().unwrap(), start.elapsed())
}

#[test]
fn sigterm_to_a_captured_or_attached_exec_stops_the_command_and_its_descendants_only() {
    let fixture = Fixture::new();
    let dir = fixture.dir.path();
    let mut sentinel = Command::new("sleep").arg("60").spawn().unwrap();
    for json in [true, false] {
        let _ = fs::remove_file(dir.join("tree"));
        let script = tree_script(dir, false);
        let args: Vec<&str> = if json { vec!["--json", "exec", "--", "sh", "-c", &script] } else { vec!["exec", "--", "sh", "-c", &script] };
        // No terminal on any stream, as for an agent or CI runner.
        let mut stack = fixture.command(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let (command, child) = started_tree(dir);
        let (status, _) = terminate(&mut stack, None);
        assert!(gone_soon(command) && gone_soon(child), "json={json}: command {command} or its child {child} outlived stack");
        let mut stdout = String::new();
        std::io::Read::read_to_string(stack.stdout.as_mut().unwrap(), &mut stdout).unwrap();
        if json {
            // The result is still reported: the command ended by the signal it was passed.
            let result: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
            assert_eq!((result["ok"].clone(), result["data"]["exit_code"].clone(), result["data"]["signal"].clone()), (json!(true), Value::Null, json!(15)), "{result}");
        }
        assert_eq!(status.code(), Some(143), "json={json}: {status:?}");
    }
    assert!(sentinel.try_wait().unwrap().is_none(), "an unrelated process was signalled");
    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

#[test]
fn a_command_ignoring_sigterm_is_killed_with_its_group_at_a_second_signal() {
    let fixture = Fixture::new();
    let dir = fixture.dir.path();
    let script = tree_script(dir, true);
    let mut stack = fixture.command(&["--json", "exec", "--", "sh", "-c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let (command, child) = started_tree(dir);
    let (_, took) = terminate(&mut stack, Some(Duration::from_millis(300)));
    assert!(took < Duration::from_secs(5), "{took:?}: a second signal must not wait out the grace period");
    assert!(gone_soon(command) && gone_soon(child), "command {command} or child {child} survived");
}

#[test]
fn sigterm_to_the_mcp_server_stops_a_running_command_answers_and_ends_the_server() {
    let fixture = Fixture::new();
    let dir = fixture.dir.path();
    let script = tree_script(dir, false);
    let mut server = fixture.command(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_exec","arguments":{"command":["sh","-c",script],"dir":dir.join("app")}}});
    use std::io::Write;
    // Input stays open: only the signal may end the server.
    let mut input = server.stdin.take().unwrap();
    writeln!(input, "{call}").unwrap();
    let (command, child) = started_tree(dir);
    let (status, _) = terminate(&mut server, None);
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(gone_soon(command) && gone_soon(child), "command {command} or child {child} outlived the server");
    let mut stdout = String::new();
    std::io::Read::read_to_string(server.stdout.as_mut().unwrap(), &mut stdout).unwrap();
    let response: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    assert_eq!(response["result"]["structuredContent"]["data"]["exit_code"], Value::Null, "{response}");
    drop(input);
}

/// A supervisor stand-in for the fake Pitchfork: a Python process (whose environment macOS lets
/// its owner read, unlike a platform binary's) running until killed, outside the test's own
/// children so it is reaped when it ends, carrying `mark` as its owner mark when given.
fn fake_supervisor(fixture: &Fixture, mark: Option<&str>) -> u32 {
    let mut command = Command::new("sh");
    command.args(["-c", r#""$0" -c 'import time; time.sleep(600)' </dev/null >/dev/null 2>&1 & echo $!"#]).arg(python::python());
    command.env_remove("STACK_SUPERVISOR_OWNER");
    if let Some(mark) = mark {
        command.env("STACK_SUPERVISOR_OWNER", mark);
    }
    let pid: u32 = String::from_utf8(command.output().unwrap().stdout).unwrap().trim().parse().unwrap();
    fs::write(fixture.dir.path().join("pf-supervisor"), pid.to_string()).unwrap();
    pid
}

/// End a stand-in a test left running.
fn end(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

#[test]
fn down_stops_the_supervisor_stack_started_by_default_once_nothing_else_uses_it() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let pf = dir.join("pf");
    let legacy = fixture.legacy_record(&pf);
    let supervisor = |args: &[&str]| -> Value {
        legacy();
        let out = fixture.command(args).output().unwrap();
        let result = json_result(&out);
        assert!(out.status.success(), "{args:?}: {result}");
        result["data"]["supervisor"].clone()
    };
    // The mark Stack 0.1.23 wrote before it first started a supervisor.
    let mark = "stack-0123456789abcdef0123456789abcdef".to_string();
    fs::write(dir.join("state/supervisor-owner"), format!("{mark}\n")).unwrap();
    let pid = fake_supervisor(&fixture, Some(&mark));
    legacy();

    // Another checkout's session records the same supervisor, through another name for its
    // state directory.
    std::os::unix::fs::symlink(&pf, dir.join("pf-alias")).unwrap();
    fs::create_dir_all(&pf).unwrap();
    let sessions = dir.join("state/sessions");
    let mine = fs::read_dir(&sessions).unwrap().next().unwrap().unwrap().path();
    let mut theirs: Value = serde_json::from_slice(&fs::read(&mine).unwrap()).unwrap();
    theirs["project"] = json!(dir.join("other"));
    theirs["provider"]["state_dir"] = json!(dir.join("pf-alias"));
    let theirs_path = sessions.join("0000000000000000.json");
    fs::write(&theirs_path, theirs.to_string()).unwrap();
    let kept = supervisor(&["down", "--json"]);
    assert_eq!(kept["stopped"], false, "{kept}");
    assert_eq!(kept["reason"], format!("the session of {} uses this supervisor", dir.join("other").display()), "{kept}");
    assert!(alive(pid));
    // Services recorded without their supervisor might be its too.
    theirs.as_object_mut().unwrap().remove("provider");
    fs::write(&theirs_path, theirs.to_string()).unwrap();
    let kept = supervisor(&["down", "--json"]);
    assert!(kept["reason"].as_str().unwrap().contains("does not record which supervisor"), "{kept}");
    // A record that cannot be read could be anyone's.
    fs::write(&theirs_path, "{").unwrap();
    let kept = supervisor(&["down", "--json"]);
    assert!(kept["reason"].as_str().unwrap().starts_with("cannot read the session record"), "{kept}");
    fs::remove_file(&theirs_path).unwrap();
    assert!(alive(pid));

    // A daemon it tracks, Stack's or not, that is not over.
    let me = std::process::id();
    for (list, reason) in [
        (json!([{ "id": "someone-else/db", "status": "running" }]), "daemon someone-else/db is running".to_string()),
        (json!([{ "id": "someone-else/db", "status": "waiting" }]), "daemon someone-else/db is waiting".to_string()),
        (json!([{ "id": "someone-else/db" }]), "daemon someone-else/db is in a state Pitchfork did not report".to_string()),
        (json!([{ "id": "someone-else/db", "status": "stopped", "pid": me }]), format!("daemon someone-else/db is stopped, but its process {me} is alive")),
    ] {
        fs::write(dir.join("pf-list.json"), list.to_string()).unwrap();
        let kept = supervisor(&["down", "--json"]);
        assert_eq!((kept["stopped"].clone(), kept["reason"].clone()), (json!(false), json!(reason)), "{list}: {kept}");
        assert!(alive(pid));
    }
    // A daemon with a schedule needs the supervisor between runs.
    let status = |id: &str, body: Value| {
        let path = dir.join("pf-status").join(format!("{id}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body.to_string()).unwrap();
    };
    let cron = "*/10 * * * * *";
    fs::write(dir.join("pf-list.json"), json!([{ "id": "someone-else/timer", "status": "stopped" }]).to_string()).unwrap();
    status("someone-else/timer", json!({ "id": "someone-else/timer", "status": "stopped", "cron_schedule": cron }));
    let kept = supervisor(&["down", "--json"]);
    assert_eq!(kept["reason"], format!("daemon someone-else/timer runs on the schedule `{cron}`, which needs the supervisor between runs"), "{kept}");
    // Without a schedule a stopped or failed daemon is over.
    fs::write(dir.join("pf-list.json"), json!([{ "id": "someone-else/db", "status": "stopped" }, { "id": "x/y", "status": "failed" }]).to_string()).unwrap();
    status("someone-else/db", json!({ "id": "someone-else/db", "status": "stopped" }));
    status("x/y", json!({ "id": "x/y", "status": "failed", "pid": null, "cron_schedule": null }));

    // Asked to keep it, through the CLI and MCP.
    assert_eq!(supervisor(&["down", "--keep-supervisor", "--json"]), json!({ "stopped": false, "reason": "asked to keep it" }));
    legacy();
    let mcp = fixture.mcp(&[("stack_down", json!({ "stop_supervisor": false }))], &[]);
    assert_eq!(mcp[0]["structuredContent"]["data"]["supervisor"], json!({ "stopped": false, "reason": "asked to keep it" }), "{}", mcp[0]);
    let out = fixture.command(&["down", "--keep-supervisor", "--stop-supervisor"]).output().unwrap();
    assert!(!out.status.success(), "the two flags contradict each other");
    assert!(alive(pid));

    // Another Stack command asking it something meanwhile holds its lock: kept.
    let held = fs::File::open(pf.join("stack-supervisor.lock")).unwrap();
    // SAFETY: flock on a descriptor this test owns.
    assert_eq!(unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&held), libc::LOCK_SH) }, 0);
    let kept = supervisor(&["down", "--json"]);
    assert_eq!(kept["reason"], "another Stack command is starting or stopping services through this supervisor", "{kept}");
    drop(held);
    assert!(alive(pid));

    // Nothing uses it: a plain `down` stops it, and its processes are gone when it returns.
    let stopped = supervisor(&["down", "--json"]);
    assert_eq!(stopped, json!({ "stopped": true, "state_dir": pf }), "{stopped}");
    assert!(!alive(pid) && !dir.join("pf-supervisor").exists());
    // Again: nothing to stop, and nothing is started to find that out.
    assert_eq!(supervisor(&["down", "--json"]), json!({ "stopped": true, "reason": "it was not running" }));

    // MCP without the field stops it by default, and the old flag still works.
    let pid = fake_supervisor(&fixture, Some(&mark));
    legacy();
    let mcp = fixture.mcp(&[("stack_down", json!({}))], &[]);
    assert_eq!(mcp[0]["structuredContent"]["data"]["supervisor"]["stopped"], true, "{}", mcp[0]);
    assert!(!alive(pid));
    let pid = fake_supervisor(&fixture, Some(&mark));
    assert_eq!(supervisor(&["down", "--stop-supervisor", "--json"])["stopped"], true);
    assert!(!alive(pid));
    // A session of Stack's own supervisor reports that supervisor gone with its services.
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let native = json_result(&fixture.ok(&["down", "--json"]))["data"]["supervisor"].clone();
    assert_eq!(native["stopped"], true, "{native}");
    assert!(native["state_dir"].as_str().unwrap().contains("/state/supervisors/"), "{native}");
}

#[test]
fn down_keeps_a_supervisor_stack_cannot_show_it_started() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let pf = dir.join("pf");
    let legacy = fixture.legacy_record(&pf);
    let supervisor = || -> Value {
        legacy();
        let out = fixture.command(&["down", "--json"]).output().unwrap();
        let result = json_result(&out);
        assert!(out.status.success(), "{result}");
        result["data"]["supervisor"].clone()
    };
    // Its own mark: tests run side by side in one process group.
    let mark = "stack-1111111111111111aaaaaaaaaaaaaaaa".to_string();
    fs::write(dir.join("state/supervisor-owner"), format!("{mark}\n")).unwrap();
    let not_started = |pid: u32| format!("this Stack state did not start supervisor pid {pid}: it was running before Stack used it, or something else started it");

    // Running before Stack used it, or started by another Stack state.
    for other in [None, Some("stack-00000000000000000000000000000000")] {
        let pid = fake_supervisor(&fixture, other);
        assert_eq!(supervisor(), json!({ "stopped": false, "reason": not_started(pid) }), "{other:?}");
        assert!(alive(pid));
        end(pid);
    }
    // Pitchfork names a process that is not the one Stack started (its PID reused, say).
    let me = std::process::id();
    fs::write(dir.join("pf-supervisor"), me.to_string()).unwrap();
    assert_eq!(supervisor(), json!({ "stopped": false, "reason": not_started(me) }));
    // Pitchfork answers but names no process.
    fs::write(dir.join("pf-supervisor"), "").unwrap();
    assert!(supervisor()["reason"].as_str().unwrap().starts_with("cannot identify the running supervisor"));

    // A replaced or missing owner record proves nothing, and is not rewritten.
    let pid = fake_supervisor(&fixture, Some(&mark));
    let record = dir.join("state/supervisor-owner");
    fs::write(&record, "not a mark\n").unwrap();
    let kept = supervisor();
    assert_eq!(kept["reason"], format!("Stack cannot tell whether it started this supervisor: {} does not hold an owner mark Stack wrote", record.display()), "{kept}");
    assert_eq!(fs::read_to_string(&record).unwrap(), "not a mark\n");
    fs::remove_file(&record).unwrap();
    assert!(supervisor()["reason"].as_str().unwrap().ends_with("supervisor-owner does not exist"));
    assert!(alive(pid));
    fs::write(&record, format!("{mark}\n")).unwrap();

    // A stop Pitchfork refuses is reported, and `down` still succeeds.
    fs::write(dir.join("pf-fail-supervisor-stop"), "").unwrap();
    assert_eq!(supervisor(), json!({ "stopped": false, "reason": "pitchfork supervisor stop: cannot stop the supervisor" }));
    assert!(alive(pid));
    fs::remove_file(dir.join("pf-fail-supervisor-stop")).unwrap();
    let stopped = supervisor();
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert!(!alive(pid));

    // A checkout without services has no supervisor to report.
    let tools = Fixture::new();
    let out = tools.ok(&["down", "--json"]);
    assert!(json_result(&out)["data"].get("supervisor").is_none(), "{}", json_result(&out));
}

#[test]
fn down_before_any_up_in_a_checkout_without_its_generated_configuration_confirms_nothing_runs() {
    let fixture = Fixture::with_bundle(WEB);
    let app = fixture.dir.path().join("app");
    // A fresh clone: stack.toml and stack.lock, its ports reserved, no generated configuration.
    fs::remove_dir_all(app.join(".config")).unwrap();
    let out = fixture.command(&["--json", "down"]).output().unwrap();
    let result = json_result(&out);
    assert!(out.status.success(), "{result}");
    assert_eq!((result["ok"].clone(), result["data"]["confirmed"].clone()), (json!(true), json!(true)), "{result}");
    assert!(!app.join(".config").exists(), "Stack's supervisor needs no generated configuration to stop anything");

    // A checkout that owns nothing (never compiled here) gets no configuration and no ports.
    let other = fixture.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::copy(app.join("stack.toml"), other.join("stack.toml")).unwrap();
    fs::copy(app.join("stack.lock"), other.join("stack.lock")).unwrap();
    let out = fixture.command_at(&other, &["--json", "down"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(!other.join(".config").exists());
    let ports = fs::read_to_string(fixture.dir.path().join("state/ports.json")).unwrap();
    assert!(!ports.contains(other.to_str().unwrap()), "{ports}");
}

#[test]
fn adding_or_editing_a_task_keeps_the_running_session_current() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let session = up["data"]["session"]["id"].clone();
    for tasks in ["[tasks.probe]\nrun='echo one'\n", "[tasks.probe]\nrun='echo two'\n[tasks.more]\nrun='true'\nservices=['web']\n"] {
        fs::write(fixture.dir.path().join("app/stack.toml"), format!("[[use]]\nbundle='path:../bundle'\n{tasks}")).unwrap();
        fixture.ok(&["compile"]);
        let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
        assert_eq!((status["data"]["healthy"].clone(), status["data"]["stale"].clone()), (json!(true), json!(false)), "{tasks}: {status}");
        let exec = json_result(&fixture.command(&["--json", "exec", "--require-all", "--", "true"]).output().unwrap());
        assert_eq!(exec["data"]["exit_code"], 0, "{tasks}: {exec}");
        let again = json_result(&fixture.ok(&["up", "--json"]));
        assert_eq!(again["data"]["session"]["id"], session, "{tasks}: the same generation keeps running: {again}");
        assert!(!again["data"]["steps"].as_array().unwrap().iter().any(|s| s["step"] == "stop_previous"), "{again}");
    }
    // A change services do see still needs a fresh start.
    fs::write(fixture.dir.path().join("app/stack.toml"), "[[use]]\nbundle='path:../bundle'\n[env]\nMODE='changed'\n").unwrap();
    fixture.ok(&["compile"]);
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["stale"], true, "{status}");
    fixture.ok(&["down"]);
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
        // Values stack checks itself are argument errors too: the same exit status, and a hint.
        &["--json", "exec", "--timeout", "1ms", "--", "true"],
        &["--json", "up", "--ttl", "banana"],
        &["--json", "up", "--timeout", "0"],
    ] {
        let out = fixture.command(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let result: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        assert_eq!(result["error"]["code"], "usage", "{args:?}");
        assert!(result["error"]["hint"].as_str().is_some_and(|h| !h.is_empty()), "{args:?}: {result}");
    }
    // The missing argument is named, not cut off after "were not provided:".
    let out = fixture.command(&["--json", "mv"]).output().unwrap();
    let error = &json_result(&out)["error"];
    assert_eq!(error["message"], "the following required arguments were not provided: <OLD> <NEW>", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("usage: stack mv <OLD> <NEW>"), "{error}");
    let mcp = fixture.mcp(&[("stack_up", json!({ "ttl": "banana" })), ("stack_exec", json!({ "command": ["true"], "timeout_secs": 0, "require": ["nope"], "require_all": true }))], &[]);
    for result in &mcp {
        let error = &result["structuredContent"]["error"];
        assert_eq!(error["code"], "usage", "{result}");
        assert!(error["hint"].as_str().is_some_and(|h| !h.is_empty()), "{result}");
    }
    // After `--`, `--json` belongs to the command, so stack's own error stays text.
    let out = fixture.command(&["exec", "--bogus", "--", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn a_failed_start_keeps_ownership_even_after_the_service_is_removed() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='exit 3'\n");
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "start_failed", "{result}");
    assert_eq!(result["error"]["details"][0]["exit"], "exit code 3", "{result}");
    assert_eq!(fixture.session()["launching"], true, "the launch stays recorded");

    // The launch record withholds the service: nothing verified it.
    let status: Value = serde_json::from_slice(&fixture.command(&["status", "--json"]).output().unwrap().stdout).unwrap();
    assert_eq!(status["data"]["checks"][0]["ready"], false);

    // Dropping the service also drops its port reservation; the launch record still names it,
    // and `down` stops what the supervisor runs for it before forgetting it.
    fs::write(fixture.dir.path().join("bundle/bundle.toml"), "[bundle]\nname='test'\n").unwrap();
    fixture.ok(&["compile"]);
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(fixture.session_paths().iter().all(|p| !p.exists() || p.is_dir()));
}

#[test]
fn a_failed_start_reports_the_services_error_without_the_supervisors_animation() {
    let run = r#"printf 'Traceback (most recent call last):\n  File "/p/server.py", line 2, in <module>\nRuntimeError: bad edit\n' >&2; exit 1"#;
    let fixture = Fixture::with_bundle(&format!("[bundle]\nname='test'\n[services.web]\nrun='''{run}'''\n"));
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "start_failed", "{result}");
    assert_eq!(result["error"]["message"], "web exited while starting (exit code 1)", "{result}");
    assert_eq!(
        result["error"]["details"][0]["output"],
        "Traceback (most recent call last):\n  File \"/p/server.py\", line 2, in <module>\nRuntimeError: bad edit",
        "{result}"
    );
    // The same lines are the service's log.
    let logs = json_result(&fixture.ok(&["logs", "web", "--json"]));
    assert_eq!(logs["data"]["lines"][2], "RuntimeError: bad edit", "{logs}");
    fixture.ok(&["down"]);
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
    fixture.sleepers();
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
    assert!(!config.contains_key("daemons"), "services are not provider config");
    fixture.set_env(&[("WEB_PORT", port.to_string()), ("STACK_IDENTITY_WEB", token.clone())]);
    fixture.sleepers();

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
    fixture.sleepers();
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
    /// A session of one service as Stack 0.1.23 left it, running under Pitchfork (the fake
    /// `pitchfork` in `bin/`): a native `up` of a listening service (see [`supervise_listener`])
    /// gives the record its digests and directory identity and is stopped, then the record is
    /// rewritten to name a detached process as the service, Pitchfork as its supervisor and
    /// `WEB_ID` as its id, and the service's port is held by a listener of the test's.
    fn start_web(&self, up_args: &[&str]) -> Running {
        let port = assigned_port(self, "web");
        supervise_listener(self, port);
        let mut args = vec!["up", "--json"];
        args.extend_from_slice(up_args);
        let up = self.command(&args).output().unwrap();
        assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stdout));
        let index = fs::read_dir(self.dir.path().join("state/sessions")).unwrap().next().unwrap().unwrap().path();
        let mut session: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        self.ok(&["down"]);
        let service = Detached::spawn();
        session["provider"] = json!({ "pitchfork": self.dir.path().join("bin/pitchfork"), "state_dir": self.dir.path().join("pfs") });
        session["services"]["web"]["pid"] = json!(service.0);
        session["services"]["web"]["provider_id"] = json!(WEB_ID);
        for path in [index, self.dir.path().join("app/.stack/session.json")] {
            fs::write(path, session.to_string()).unwrap();
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
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

    /// Make the bundle's `run='true'` services processes that run until stopped without
    /// listening (see [`SLEEPER`]): the test then stands in for what they serve, binding their
    /// ports once `started` exists. Compiles again; ports are kept.
    fn sleepers(&self) {
        let path = self.dir.path().join("bundle/bundle.toml");
        let bundle = fs::read_to_string(&path).unwrap();
        fs::write(&path, bundle.replace("run='true'", &format!("run='''{SLEEPER}'''"))).unwrap();
        self.ok(&["compile"]);
    }

    /// A closure that records this checkout's session as Stack 0.1.23 left it under the
    /// Pitchfork supervisor of state directory `pf` (the fake `pitchfork` in `bin/`), with
    /// nothing of it running any more: what `down` then decides about that supervisor. The
    /// record's digests and directory identity come from a native `up` of a listening service,
    /// stopped before this returns.
    fn legacy_record(&self, pf: &Path) -> impl Fn() + '_ {
        supervise_listener(self, assigned_port(self, "web"));
        self.ok(&["up"]);
        let index = fs::read_dir(self.dir.path().join("state/sessions")).unwrap().next().unwrap().unwrap().path();
        let mut session: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        self.ok(&["down"]);
        session["provider"] = json!({ "pitchfork": self.dir.path().join("bin/pitchfork"), "state_dir": pf });
        session["services"]["web"].as_object_mut().unwrap().remove("pid");
        session["services"]["web"]["provider_id"] = json!(WEB_ID);
        let mirror = self.dir.path().join("app/.stack/session.json");
        move || {
            for path in [&index, &mirror] {
                fs::write(path, session.to_string()).unwrap();
            }
        }
    }

    /// Make the bundle's `web` a Postgres preset served by the stand-ins of
    /// `support/servers.rs` (installed in `bin/`), and compile again; its port is kept.
    fn postgres_web(&self) -> u16 {
        servers::install(&self.dir.path().join("bin"));
        let path = self.dir.path().join("bundle/bundle.toml");
        let bundle = fs::read_to_string(&path).unwrap();
        fs::write(&path, bundle.replace("[services.web]\nrun='true'", "[services.web]\npreset='postgres'\nversion='17'")).unwrap();
        self.ok(&["compile"]);
        let port = assigned_port(self, "web");
        // What mise renders from the generated configuration's `[env]` (the fake answers from
        // `env.json`).
        self.set_env(&[("DATABASE_URL", format!("postgresql://postgres@127.0.0.1:{port}/postgres"))]);
        port
    }

    /// `web`'s data directory where mise's preset kept it for this checkout, as an older Stack
    /// left it (`<MISE_STATE_DIR>/daemons/<hash>/data/web`, its `state.json` naming the
    /// checkout), initialised for Postgres 17.
    fn legacy_data(&self, hash: &str) -> std::path::PathBuf {
        let state = self.dir.path().join("mise-state/daemons").join(hash);
        let data = state.join("data/web");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(".mise-daemon-version"), "postgres:17").unwrap();
        let app = self.dir.path().join("app").canonicalize().unwrap();
        fs::write(state.join("state.json"), json!({ "root": app }).to_string()).unwrap();
        data
    }

    /// Have Stack's supervisor directory `dir` report `service` running as `pid`.
    fn report_running(&self, dir: &Path, service: &str, pid: u32) {
        let started = stack::procinfo::process(pid).unwrap().started;
        let boot = stack::procinfo::boot_id();
        let state = json!({ "supervisor": null, "applied": 1, "services": { service: { "status": "running", "process": { "pid": pid, "started": started, "boot": boot }, "port": 1, "stop_signal": 15 } } });
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("state.json"), state.to_string()).unwrap();
    }

    /// What Stack's supervisor for the checkout last wrote (its `state.json`).
    fn supervisor_state(&self) -> Value {
        let dir = fs::read_dir(self.dir.path().join("state/supervisors")).unwrap().next().unwrap().unwrap().path();
        serde_json::from_slice(&fs::read(dir.join("state.json")).unwrap()).unwrap()
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
        assert_eq!(out.status.code(), Some(70), "{args:?}: a Stack error exits 70");
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
    // The command signals once it runs: stack has registered it and finished writing the
    // project before starting it, so deleting the project cannot race that write.
    let started = fixture.dir.path().join("exec-started");
    let script = format!("touch '{}'; sleep 3", started.display());
    let mut child = fixture.command(&["exec", "--", "sh", "-c", &script]).stdout(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(Instant::now() < deadline, "execution never started");
        thread::sleep(Duration::from_millis(10));
    }
    let session = fs::read_to_string(fixture.dir.path().join("app/.stack/session.json")).unwrap();
    assert!(session.contains("active_executions\": {\n    \""), "{session}");
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
    assert!(passes.iter().all(|p| p["command"] == "gc" && p["passed"] == true && p["data"]["pass"].is_u64()), "{passes:?}");
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
fn every_partial_start_failure_preserves_observed_ownership() {
    // One service runs (without listening, so it never verifies), the other exits at once.
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\n[services.other]\nrun='false'\n");
    fixture.sleepers();
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(json_result(&out)["error"]["code"], "start_failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.dir.path().join("pf-tracked-pid").exists() {
        assert!(Instant::now() < deadline, "web never started");
        thread::sleep(Duration::from_millis(10));
    }
    let web: u64 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    for file in fixture.session_paths() {
        let record: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        assert_eq!(record["launching"], true, "{record}");
        assert_eq!(record["services"]["web"]["pid"], web, "{record}");
        assert!(record["services"]["other"]["pid"].is_null(), "{record}");
        assert_eq!(record["provider"]["supervisor"], "stack", "{record}");
    }
    fixture.ok(&["down"]);
    assert!(!pid_alive(web as u32));
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

/// A service command for Stack's supervisor that runs until stopped without listening, recording
/// that it started (`started`) and its pid (`pf-tracked-pid`).
const SLEEPER: &str = r#"echo $$ >"$REVIEW_FIXTURE/pf-tracked-pid"; touch "$REVIEW_FIXTURE/started"; exec sleep 600"#;

/// A service command for Stack's supervisor: listens on `$PORT` until stopped, recording its
/// pid in `pf-tracked-pid`. With `slow-start`, it first waits that many seconds.
const LISTEN: &str = r#"if test -f "$REVIEW_FIXTURE/slow-start"; then sleep "$(cat "$REVIEW_FIXTURE/slow-start")"; fi; exec "$REVIEW_PYTHON" -c 'import os,socket,time; open(os.environ["REVIEW_FIXTURE"]+"/pf-tracked-pid","w").write(str(os.getpid())); s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(os.environ["PORT"]))); s.listen(); time.sleep(600)'"#;

/// Make the bundle's services listen on their ports (see [`LISTEN`]) instead of exiting at
/// once, and compile again. `port` is the service's assigned port, which this keeps.
fn supervise_listener(fixture: &Fixture, port: u16) {
    let path = fixture.dir.path().join("bundle/bundle.toml");
    let bundle = fs::read_to_string(&path).unwrap();
    fs::write(&path, bundle.replace("run='true'", &format!("run='''{LISTEN}'''"))).unwrap();
    fixture.ok(&["compile"]);
    assert_eq!(assigned_port(fixture, "web"), port);
}

#[test]
fn a_killed_service_is_reported_as_not_running_not_as_a_changed_process() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let pid: i32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid as u32) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    let reason = status["data"]["checks"][0]["reason"].as_str().unwrap_or_else(|| panic!("{status}")).to_string();
    assert!(reason.contains("supervisor reports 'failed'"), "{reason}");
    assert!(reason.contains("is gone") && reason.ends_with("run `stack up` to start it again"), "{reason}");
    assert!(!reason.contains("changed since launch"), "{reason}");
    fixture.ok(&["down"]);
}

#[test]
fn a_declared_readiness_check_holds_the_start_until_it_passes() {
    let fixture = Fixture::with_bundle(&format!("[bundle]\nname='test'\n[services.web]\nrun='''{LISTEN}'''\nready_cmd='test -f ready && test -n \"$PORT\"'\n"));
    let out = fixture.command(&["up", "--timeout", "5s", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "timed_out", "a listening service is not ready before its check passes: {result}");
    // The check that never passed is named, with how it answers, instead of only "keep waiting".
    let hint = result["error"]["hint"].as_str().unwrap();
    assert!(hint.starts_with("web's ready_cmd exits 1") && hint.contains("stack logs") && hint.contains("stack down"), "{hint}");
    let cause = &result["error"]["details"][0]["cause"]["details"];
    let readiness = &cause.as_array().unwrap().iter().find_map(|d| d.get("readiness")).unwrap_or_else(|| panic!("{result}"))[0];
    assert_eq!((readiness["service"].clone(), readiness["exit_code"].clone()), (json!("web"), json!(1)), "{result}");
    assert_eq!(readiness["ready_cmd"], "test -f ready && test -n \"$PORT\"", "{result}");
    fixture.ok(&["down"]);
    // A check that cannot run at all (no `sh` on the stack's PATH) never passes, and the
    // report says why it did not run, rather than nothing.
    fs::write(fixture.dir.path().join("app/ready"), "").unwrap();
    let env = fs::read(fixture.dir.path().join("env.json")).unwrap();
    let no_shell = fixture.dir.path().join("no-shell");
    fs::create_dir(&no_shell).unwrap();
    let python_dir = Path::new(&python::python()).parent().unwrap().to_path_buf();
    fs::write(fixture.dir.path().join("env.json"), json!({ "PATH": format!("{}:{}", no_shell.display(), python_dir.display()) }).to_string()).unwrap();
    let result = json_result(&fixture.command(&["up", "--timeout", "5s", "--json"]).output().unwrap());
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    let hint = result["error"]["hint"].as_str().unwrap();
    assert!(hint.starts_with("web's ready_cmd could not be run (cannot run it through sh"), "{hint}");
    fs::write(fixture.dir.path().join("env.json"), env).unwrap();
    fixture.ok(&["down"]);
    let started = Instant::now();
    let up = json_result(&fixture.ok(&["up", "--timeout", "2m", "--json"]));
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    fixture.ok(&["down"]);
}

#[test]
fn gc_releases_a_removed_checkouts_ports_and_removes_its_data_only_when_asked_and_safe() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let app = dir.join("app");
    let port = fixture.postgres_web();
    let data = fixture.legacy_data("0123");
    fs::write(data.join("rows"), "seeded").unwrap();
    let token = stack::checkout::token(&app).unwrap();
    fixture.ok(&["up"]);
    fixture.ok(&["down"]);
    // `up` recorded where the data is, whose it is, which directory it is (where it resolves,
    // device and inode) and which supervisor runs the service.
    let ledger: Value = serde_json::from_slice(&fs::read(dir.join("state/data.json")).unwrap()).unwrap();
    assert_eq!(ledger["entries"][0]["checkout"], token.as_str(), "{ledger}");
    assert_eq!(ledger["entries"][0]["supervisor"]["id"], "web", "{ledger}");
    assert!(ledger["entries"][0]["supervisor"]["state_dir"].as_str().unwrap().contains("/state/supervisors/"), "{ledger}");
    assert_eq!(ledger["entries"][0]["location"], json!(data.canonicalize().unwrap()), "{ledger}");
    assert!(ledger["entries"][0]["identity"].is_array(), "{ledger}");
    let recorded = ledger["entries"][0].clone();
    // A checkout that still exists keeps its data, even when asked.
    let out = fixture.command_at(dir, &["gc", "--reclaim-data", "--json"]).output().unwrap();
    assert!(out.status.success() && data.join("rows").exists(), "{}", json_result(&out));
    // Removed after a confirmed `down`, as a worktree manager's hook does.
    fs::remove_dir_all(&app).unwrap();
    let gc = |args: &[&str]| -> Vec<String> {
        let out = fixture.command_at(dir, args).output().unwrap();
        let result = json_result(&out);
        assert!(out.status.success(), "{result}");
        result["data"].as_array().unwrap().iter().filter(|e| e["project"].as_str().is_some_and(|p| p.ends_with("/app"))).flat_map(|e| e["services"].as_array().unwrap().clone()).map(|s| s["outcome"].as_str().unwrap().to_string()).collect()
    };
    let ports = || fs::read_to_string(dir.join("state/ports.json")).unwrap();
    assert!(ports().contains(app.to_str().unwrap()));

    // Ports come back at once; the data is only reported.
    let outcomes = gc(&["gc", "--json"]);
    assert!(outcomes.contains(&format!("port {port} released")), "{outcomes:?}");
    assert!(outcomes.iter().any(|o| o.starts_with("data kept in") && o.ends_with("`stack gc --reclaim-data` removes it")), "{outcomes:?}");
    assert!(!ports().contains(app.to_str().unwrap()), "{}", ports());
    assert!(data.join("rows").exists());

    // A process still carrying the deleted checkout's token keeps it, even when asked.
    // Not a macOS platform binary such as /bin/sleep, whose environment nobody may read.
    let mut carrier = Command::new(python::python()).args(["-c", "import time; time.sleep(30)"]).env("STACK_CHECKOUT_TOKEN", &token).spawn().unwrap();
    thread::sleep(Duration::from_millis(300));
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    let _ = carrier.kill();
    let _ = carrier.wait();
    assert!(outcomes.iter().any(|o| o.contains("still runs") && o.contains(&carrier.id().to_string())), "{outcomes:?}");
    assert!(data.join("rows").exists());

    // Nor while the recorded supervisor still reports the service running.
    let mut busy = Command::new("sleep").arg("30").spawn().unwrap();
    let supervisor_dir = std::path::PathBuf::from(recorded["supervisor"]["state_dir"].as_str().unwrap());
    fixture.report_running(&supervisor_dir, "web", busy.id());
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    let _ = busy.kill();
    let _ = busy.wait();
    assert!(outcomes.iter().any(|o| o.contains("Stack's supervisor reports web running")), "{outcomes:?}");
    assert!(data.join("rows").exists());

    // Data whose marker now names another checkout is never touched, and no longer reported.
    let marker = data.with_file_name("web.stack-checkout");
    let mine = fs::read(&marker).unwrap();
    fs::write(&marker, json!({ "checkout": "another-checkout" }).to_string()).unwrap();
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    assert!(outcomes.iter().any(|o| o.contains("no longer carries this checkout's marker")), "{outcomes:?}");
    assert!(data.join("rows").exists());
    assert!(gc(&["gc", "--reclaim-data", "--json"]).is_empty(), "settled");

    // From here the supervisor the record names reports nothing for it, so only where the
    // directory is and which one it is decide.
    fs::write(&marker, mine).unwrap();
    let mut entry = recorded.clone();
    entry["supervisor"] = json!({ "pitchfork": dir.join("bin/pitchfork"), "state_dir": dir.join("pf"), "id": "app-test/web" });
    let record = || fs::write(dir.join("state/data.json"), json!({ "entries": [entry.clone()] }).to_string()).unwrap();
    let parent = data.parent().unwrap().to_path_buf();
    let (original, backup) = (dir.join("original-moved"), dir.join("independent-backup"));
    let copy = |from: &Path, to: &Path| assert!(Command::new("cp").arg("-Rp").arg(from).arg(to).status().unwrap().success());
    let untouched = |root: &Path, case: &str| {
        assert_eq!(fs::read_to_string(root.join("web/rows")).unwrap(), "seeded", "{case}: data under {}", root.display());
        assert!(root.join("web.stack-checkout").exists(), "{case}: marker under {}", root.display());
    };
    fs::write(parent.join("unsaved.txt"), "keep me").unwrap();

    // The recorded directory's parent moved away and replaced by a link to an independent
    // copy, marker and all, with a file of its own: neither is touched.
    fs::rename(&parent, &original).unwrap();
    copy(&original, &backup);
    fs::write(backup.join("web/backup-only"), "only here").unwrap();
    std::os::unix::fs::symlink(&backup, &parent).unwrap();
    record();
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    assert!(outcomes.iter().any(|o| o.contains("left alone") && o.contains("link")), "{outcomes:?}");
    untouched(&original, "redirected parent");
    untouched(&backup, "redirected parent");
    assert!(backup.join("web/backup-only").exists());
    fs::remove_file(&parent).unwrap();

    // A copy put in its place, marker and all, with no link: not the directory `up` saw.
    copy(&original, &parent);
    record();
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    assert!(outcomes.iter().any(|o| o.contains("left alone") && o.contains("not the directory")), "{outcomes:?}");
    untouched(&parent, "copied in place");
    untouched(&original, "copied in place");
    fs::remove_dir_all(&parent).unwrap();

    // The recorded directory back where it was: asked, with the marker its own and nothing
    // running, it is removed with its marker, once, and nothing beside it.
    fs::rename(&original, &parent).unwrap();
    record();
    let outcomes = gc(&["gc", "--json"]);
    assert!(outcomes.iter().any(|o| o.starts_with("data kept in") && o.ends_with("`stack gc --reclaim-data` removes it")), "{outcomes:?}");
    untouched(&parent, "not asked");
    let outcomes = gc(&["gc", "--reclaim-data", "--json"]);
    assert!(outcomes.iter().any(|o| o.starts_with(&format!("data in {} removed", data.display()))), "{outcomes:?}");
    assert!(!data.exists() && !marker.exists());
    assert_eq!(fs::read_to_string(parent.join("unsaved.txt")).unwrap(), "keep me", "only the recorded directory is removed");
    untouched(&backup, "the independent copy");
    assert!(gc(&["gc", "--json"]).is_empty());
}

#[test]
fn a_services_setup_runs_before_each_start_and_a_failing_one_stops_it() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nsetup='echo \"setup port=$PORT\" >> setup.log'\n");
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    let log = fixture.dir.path().join("app/setup.log");
    fixture.ok(&["up"]);
    assert_eq!(fs::read_to_string(&log).unwrap(), format!("setup port={port}\n"));
    // A running service with unchanged configuration is not set up again.
    fixture.ok(&["up"]);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    // A restart starts it again, after its setup.
    fixture.ok(&["restart", "web"]);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 2);
    fixture.ok(&["down"]);

    // A failing setup: the service is not requested, and the error says what the setup said.
    fs::write(fixture.dir.path().join("bundle/bundle.toml"), "[bundle]\nname='test'\n[services.web]\nrun='true'\nsetup='echo migrating; echo boom >&2; exit 4'\n").unwrap();
    fixture.ok(&["compile"]);
    let _ = fs::remove_file(fixture.dir.path().join("start-requests.log"));
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    let error = json_result(&out)["error"].clone();
    assert_eq!(error["code"], "setup_failed", "{error}");
    assert_eq!(error["message"], "web's setup exited 4, so web was not started", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("stack exec -- sh -c"), "{error}");
    let detail = error["details"].as_array().unwrap().iter().find(|d| d.get("setup").is_some()).unwrap().clone();
    assert_eq!((detail["exit_code"].clone(), detail["stdout"].clone(), detail["stderr"].clone()), (json!(4), json!("migrating"), json!("boom")), "{error}");
    assert!(!fixture.dir.path().join("start-requests.log").exists(), "web was requested");
    fixture.ok(&["down"]);
}

#[test]
fn a_service_is_requested_only_once_the_services_it_depends_on_are_ready() {
    // Declared first, but it waits for its dependency, which never becomes ready here.
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.api]\nrun='true'\ndepends_on=['db']\n[services.db]\nrun='true'\n");
    let out = fixture.command(&["up", "--timeout", "3s", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert!(!out.status.success());
    assert!(result.to_string().contains("\"not_started\":[\"api\"]"), "{result}");
    let state = fixture.supervisor_state();
    assert!(state["services"]["db"].is_object() && state["services"].get("api").is_none(), "{state}");
    fixture.ok(&["down"]);
}

#[test]
fn up_again_still_checks_that_a_running_dependency_passes_its_readiness() {
    let fixture = Fixture::with_bundle(&format!(
        "[bundle]\nname='test'\n[services.db]\nrun='''{LISTEN}'''\nready_cmd='test -f db-ready'\n[services.api]\nrun='''{LISTEN}'''\ndepends_on=['db']\n"
    ));
    let app = fixture.dir.path().join("app");
    fs::write(app.join("db-ready"), "").unwrap();
    fixture.ok(&["up"]);
    // Both still run and verify, so a second `up` has nothing to start.
    let again = json_result(&fixture.ok(&["up", "--json"]));
    assert!(again["data"]["checks"].as_array().unwrap().iter().all(|c| c["ready"] == true), "{again}");

    // db still runs but no longer passes its ready_cmd: api must not be reported started on it.
    fs::remove_file(app.join("db-ready")).unwrap();
    let out = fixture.command(&["up", "--timeout", "5s", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert!(!out.status.success(), "{result}");
    assert_eq!(result["ok"], false, "{result}");
    assert!(result["error"].to_string().contains("db"), "{result}");
    fs::write(app.join("db-ready"), "").unwrap();
    fixture.ok(&["down"]);
}

#[test]
fn restarting_a_dependent_alone_first_checks_what_it_depends_on_and_changes_nothing_when_it_is_not_ready() {
    let fixture = Fixture::with_bundle(&format!(
        "[bundle]\nname='test'\n[services.db]\nrun='''{LISTEN}'''\nready_cmd='test -f db-ready'\n[services.api]\nrun='''{LISTEN}'''\ndepends_on=['db']\nsetup='echo setup >> setup.log'\n"
    ));
    let (dir, app) = (fixture.dir.path().to_path_buf(), fixture.dir.path().join("app"));
    fs::write(app.join("db-ready"), "").unwrap();
    fixture.ok(&["up"]);
    let log = || fs::read_to_string(app.join("setup.log")).unwrap();
    let pid_of = |name: &str| -> u32 { fixture.session()["services"][name]["pid"].as_u64().unwrap() as u32 };
    assert_eq!(log(), "setup\n");
    let pid = pid_of("api");

    // db alive but failing its ready_cmd, then db stopped: `restart api` stops, sets up and
    // starts nothing, and says why.
    let refused = |case: &str| {
        let session = fs::read(app.join(".stack/session.json")).unwrap();
        let out = fixture.command(&["restart", "api", "--json"]).output().unwrap();
        let error = json_result(&out)["error"].clone();
        assert!(!out.status.success(), "{case}: {error}");
        assert_eq!(error["code"], "dependency_not_ready", "{case}: {error}");
        assert!(error["message"].as_str().unwrap().contains("api depends on db"), "{case}: {error}");
        assert!(error["hint"].as_str().unwrap().contains("stack restart db"), "{case}: {error}");
        assert_eq!(log(), "setup\n", "{case}: api was set up");
        assert!(pid_alive(pid), "{case}: the running api was stopped");
        assert_eq!(fs::read(app.join(".stack/session.json")).unwrap(), session, "{case}: the session record changed");
        error
    };
    fs::remove_file(app.join("db-ready")).unwrap();
    let error = refused("db not ready");
    assert!(error.to_string().contains("ready_cmd"), "{error}");
    // The same while db's ready_cmd cannot run at all (no `sh` on the stack's PATH): a check
    // that answered nothing does not say db is ready, and why it did not run is reported.
    let env = fs::read(dir.join("env.json")).unwrap();
    let no_shell = dir.join("no-shell");
    fs::create_dir(&no_shell).unwrap();
    fs::write(dir.join("env.json"), json!({ "PATH": no_shell }).to_string()).unwrap();
    let error = refused("db's ready_cmd cannot run");
    assert!(error["message"].as_str().unwrap().contains("could not be run"), "{error}");
    assert!(error.to_string().contains("cannot run it through sh"), "{error}");
    fs::write(dir.join("env.json"), env).unwrap();
    fs::write(app.join("db-ready"), "").unwrap();
    // db's process gone.
    let db = pid_of("db");
    unsafe { libc::kill(db as i32, libc::SIGKILL) };
    let gone = Instant::now() + Duration::from_secs(5);
    while pid_alive(db) && Instant::now() < gone {
        thread::sleep(Duration::from_millis(20));
    }
    refused("db stopped");

    // `up` starts db again and, since api depended on the dead one, sets api up and starts it
    // after it; then the same restart as above sets api up and starts it anew.
    fixture.ok(&["up"]);
    assert_eq!(log(), "setup\nsetup\n");
    let pid = pid_of("api");
    let restarted = json_result(&fixture.ok(&["restart", "api", "--json"]));
    assert_eq!(restarted["data"]["restarted"], json!(["api"]), "{restarted}");
    assert_eq!(log(), "setup\nsetup\nsetup\n");
    assert_ne!(pid_of("api"), pid, "api was not started anew");
    assert!(!pid_alive(pid));
    fixture.ok(&["down"]);
}

/// A bundle whose `web` setup records its PID in `setup-ready` and waits; on SIGTERM it exits 0
/// (`graceful`), or ignores it. `api`'s setup, run after it, would log to `api-setup.log`.
fn cancellable_setup(graceful: bool) -> Fixture {
    let trap = if graceful { "trap \"exit 0\" TERM" } else { "trap \"\" TERM" };
    Fixture::with_bundle(&format!(
        "[bundle]\nname='test'\n[services.web]\nrun='true'\nsetup='{trap}; echo $$ > setup-ready; sleep 30 & wait; echo after-wait >> setup-ready'\n[services.api]\nrun='true'\nsetup='echo api >> api-setup.log'\n"
    ))
}

/// The setup's PID, once it is waiting.
fn setup_waiting(app: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(pid) = fs::read_to_string(app.join("setup-ready")).ok().and_then(|s| s.lines().next()?.trim().parse().ok()) {
            return pid;
        }
        assert!(Instant::now() < deadline, "setup never started");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_signal_during_setup_cancels_the_start_however_the_setup_ends() {
    let mut sentinel = Command::new("sleep").arg("60").spawn().unwrap();
    // A setup that exits 0 on SIGTERM, then one that ignores it until a second signal.
    for graceful in [true, false] {
        let fixture = cancellable_setup(graceful);
        let (dir, app) = (fixture.dir.path(), fixture.dir.path().join("app"));
        let mut stack = fixture.command(&["--json", "up", "--timeout", "1m"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let setup = setup_waiting(&app);
        let (status, _) = terminate(&mut stack, (!graceful).then_some(Duration::from_millis(300)));
        let mut stdout = String::new();
        std::io::Read::read_to_string(stack.stdout.as_mut().unwrap(), &mut stdout).unwrap();
        let result: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("graceful={graceful}: {e}: {stdout:?}"));
        assert_eq!(status.code(), Some(143), "graceful={graceful}: {status:?} {result}");
        let error = &result["error"];
        assert_eq!(error["code"], "cancelled", "graceful={graceful}: {result}");
        assert!(error["message"].as_str().unwrap().contains("web's setup"), "{result}");
        assert!(gone_soon(setup), "graceful={graceful}: setup {setup} outlived stack");
        // Nothing after the cancelled setup ran: no other setup, no start request.
        assert!(!app.join("api-setup.log").exists(), "graceful={graceful}: api's setup ran after the cancel");
        assert!(!dir.join("start-requests.log").exists(), "graceful={graceful}: a service was requested after the cancel");
        // The launch stays recorded, unverified, for `down`.
        let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
        assert_eq!(status["data"]["healthy"], false, "{status}");
        assert_eq!(status["data"]["session"]["launching"], true, "{status}");
        fixture.ok(&["down"]);
    }
    assert!(sentinel.try_wait().unwrap().is_none(), "an unrelated process was signalled");
    let _ = sentinel.kill();
    let _ = sentinel.wait();
}

#[test]
fn a_signal_to_the_mcp_server_during_setup_cancels_the_start_and_ends_the_server() {
    let fixture = cancellable_setup(true);
    let (dir, app) = (fixture.dir.path(), fixture.dir.path().join("app"));
    let mut server = fixture.command(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stack_up","arguments":{"dir":app,"timeout_secs":60}}});
    use std::io::Write;
    // Input stays open: only the signal may end the server.
    let mut input = server.stdin.take().unwrap();
    writeln!(input, "{call}").unwrap();
    let setup = setup_waiting(&app);
    let (status, _) = terminate(&mut server, None);
    assert_eq!(status.code(), Some(143), "{status:?}");
    let mut stdout = String::new();
    std::io::Read::read_to_string(server.stdout.as_mut().unwrap(), &mut stdout).unwrap();
    let response: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    let envelope = &response["result"]["structuredContent"];
    assert_eq!(envelope["error"]["code"], "cancelled", "{response}");
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(gone_soon(setup), "setup {setup} outlived the server");
    assert!(!app.join("api-setup.log").exists() && !dir.join("start-requests.log").exists(), "work went on after the cancel");
    drop(input);
    fixture.ok(&["down"]);
}

#[test]
fn data_a_previous_checkout_at_this_path_left_is_moved_aside_and_this_checkouts_is_kept() {
    let fixture = Fixture::with_bundle(WEB);
    fixture.postgres_web();
    // Data mise's preset kept for this path before markers existed is found and adopted.
    let data = fixture.legacy_data("0123");
    let marker = data.with_file_name("web.stack-checkout");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    let aside = || fs::read_dir(data.parent().unwrap()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("web.replaced-")).collect::<Vec<_>>();
    fs::write(data.join("legacy"), "").unwrap();
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(up["data"].get("warnings").is_none(), "{up}");
    assert!(data.join("legacy").exists());
    assert!(!fixture.dir.path().join("initdb.log").exists(), "existing data is never initialised again");
    let owner: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert_eq!(owner["checkout"], token.as_str());
    fixture.ok(&["down"]);

    // The same checkout keeps its data across restarts.
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(up["data"].get("warnings").is_none() && data.join("legacy").exists() && aside().is_empty(), "{up}");
    fixture.ok(&["down"]);

    // Data another checkout at this path left is moved aside with its marker, and said so.
    fs::write(&marker, json!({ "checkout": "a-deleted-checkout" }).to_string()).unwrap();
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let warning = up["data"]["warnings"][0].as_str().unwrap_or_else(|| panic!("{up}"));
    assert!(warning.starts_with("web: data an earlier checkout") && warning.contains("starts with empty data"), "{warning}");
    let moved = aside();
    assert_eq!(moved.len(), 2, "the directory and its marker: {moved:?}");
    let moved_dir = data.with_file_name(moved.iter().find(|n| !n.ends_with(".stack-checkout")).unwrap());
    assert!(moved_dir.join("legacy").exists(), "kept, not deleted");
    assert!(!data.join("legacy").exists(), "this checkout starts empty");
    assert!(fs::read_to_string(fixture.dir.path().join("initdb.log")).unwrap().contains("-U postgres --auth=trust"), "a new cluster");
    assert_eq!(fs::read_to_string(data.join(".mise-daemon-version")).unwrap(), "postgres:17");
    let owner: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert_eq!(owner["checkout"], token.as_str());
    fixture.ok(&["down"]);
}

#[test]
fn down_confirms_only_once_a_lost_service_is_gone_even_when_it_outlives_its_port() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    // Its listener closes the port on SIGTERM, keeps running, and is reparented once its
    // wrapper dies: still the same process, so it is killed before anything is confirmed.
    let lost = lost_service_with(port, &token, true, false);
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(lost.listener), "confirmed while the listener lived");
}

#[test]
fn down_confirms_only_once_every_process_of_a_lost_service_is_gone() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    // A worker beside the listener ignores SIGTERM and holds no port.
    let lost = lost_service_with(port, &token, false, true);
    let worker = lost.worker.unwrap();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(worker), "confirmed while the worker lived");
    assert!(down["data"]["stopped"].as_array().unwrap().iter().any(|s| s["pid"] == worker), "{down}");
}

#[test]
fn down_stops_a_process_a_lost_service_starts_while_it_stops() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    let respawned = fixture.dir.path().join("respawned");
    std::env::set_var("LOST_RESPAWNED", &respawned);
    // Its listener starts a worker, carrying its environment, as SIGTERM ends it.
    let lost = lost_service_with_mode(port, &token, "2", false);
    let down = json_result(&fixture.ok(&["down", "--json"]));
    let worker: u32 = fs::read_to_string(&respawned).unwrap().trim().parse().unwrap();
    let cleanup = Lost { wrapper: worker, listener: worker, worker: Some(worker) };
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(worker), "confirmed while the worker it started lived: {down}");
    drop((cleanup, lost));
}

#[test]
fn down_stops_a_service_child_that_keeps_the_port_after_the_supervisor_stops_its_leader() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    supervise_listener(&fixture, port);
    fixture.ok(&["up"]);
    // The supervisor's process for the service is now a wrapper whose child holds the port;
    // stopping the wrapper leaves the child listening.
    let tracked = fixture.dir.path().join("pf-tracked-pid");
    let old: i32 = fs::read_to_string(&tracked).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(old, libc::SIGKILL) };
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        thread::sleep(Duration::from_millis(50));
    }
    let lost = lost_service(port, &token);
    fs::write(&tracked, lost.wrapper.to_string()).unwrap();
    let started = Instant::now();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(started.elapsed() < Duration::from_secs(15), "waited {:?} before stopping it", started.elapsed());
    assert!(!pid_alive(lost.wrapper) && !pid_alive(lost.listener), "{down}");
}

/// A service process the supervisor launched and then lost track of, as Pitchfork 2.29.0 did
/// with a start it was still recording when it was told to stop: a wrapper leading its own
/// session (as Pitchfork starts each service) with a child listening on `port`, both with
/// `token` in their environment, orphaned to init. Killed with its session when dropped.
struct Lost {
    wrapper: u32,
    listener: u32,
    /// A worker beside the listener, in the same group, that ignores SIGTERM, when asked for.
    worker: Option<u32>,
}

impl Drop for Lost {
    fn drop(&mut self) {
        // SAFETY: the test's own processes: the session the wrapper leads, and its worker.
        unsafe { libc::kill(-(self.wrapper as i32), libc::SIGKILL) };
        if let Some(worker) = self.worker {
            unsafe { libc::kill(worker as i32, libc::SIGKILL) };
        }
    }
}

fn lost_service(port: u16, token: &str) -> Lost {
    lost_service_with(port, token, false, false)
}

/// [`lost_service`]; a `stubborn` listener closes its port on SIGTERM but keeps running, and
/// `worker` adds a process beside it.
fn lost_service_with(port: u16, token: &str, stubborn: bool, worker: bool) -> Lost {
    lost_service_with_mode(port, token, if stubborn { "1" } else { "0" }, worker)
}

/// [`lost_service_with`] by the listener's SIGTERM `mode`: "0" ends, "1" closes its port and
/// keeps running, "2" starts a worker (its PID written to `$LOST_RESPAWNED`) and ends.
fn lost_service_with_mode(port: u16, token: &str, mode: &str, worker: bool) -> Lost {
    let script = r#"
import os, subprocess, sys
if sys.argv[1] == "launch":
    wrapper = subprocess.Popen([sys.executable, "-c", sys.argv[2], "wrap", sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]], start_new_session=True, stdout=subprocess.PIPE, text=True)
    print(wrapper.pid, wrapper.stdout.readline().strip(), flush=True)
    os._exit(0)
listen = """
import os, signal, socket, subprocess, sys, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', int(sys.argv[1]))); s.listen()
def respawn(*_):
    worker = subprocess.Popen([sys.executable, '-c', 'import time; [time.sleep(1) for _ in range(600)]'])
    open(os.environ['LOST_RESPAWNED'], 'w').write(str(worker.pid))
    os._exit(0)
if sys.argv[2] == '1': signal.signal(signal.SIGTERM, lambda *_: s.close())
if sys.argv[2] == '2': signal.signal(signal.SIGTERM, respawn)
print('ready', flush=True)
[time.sleep(1) for _ in range(600)]
"""
child = subprocess.Popen([sys.executable, "-c", listen, sys.argv[3], sys.argv[4]], stdout=subprocess.PIPE, text=True)
child.stdout.readline()
worker = None
if sys.argv[5] == "1":
    worker = subprocess.Popen([sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready', flush=True); [time.sleep(1) for _ in range(600)]"], stdout=subprocess.PIPE, text=True)
    worker.stdout.readline()
print(child.pid, worker.pid if worker else 0, flush=True)
child.wait()
"#;
    let out = Command::new(python::python())
        .args(["-c", script, "launch", script, &port.to_string(), mode, if worker { "1" } else { "0" }])
        .env("STACK_CHECKOUT_TOKEN", token)
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = text.split_whitespace().map(|p| p.parse::<u32>().unwrap_or_else(|_| panic!("{text}")));
    Lost { wrapper: pids.next().unwrap(), listener: pids.next().unwrap(), worker: pids.next().filter(|p| *p != 0) }
}

#[test]
fn a_service_process_the_supervisor_lost_is_stopped_by_up_and_down_never_called_foreign() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    let token = stack::checkout::token(&fixture.dir.path().join("app")).unwrap();
    supervise_listener(&fixture, port);

    // `up` stops it instead of refusing with port_conflict, and starts the service: while
    // reconciling a missing launch record, or, with a record, before checking ports.
    let lost = lost_service(port, &token);
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let step = |up: &Value, name: &str| up["data"]["steps"].as_array().unwrap().iter().find(|s| s["step"] == name).unwrap().clone();
    let stopped = json!([{ "service": "web", "port": port, "pid": lost.listener, "unrecorded": true }]);
    assert_eq!(step(&up, "stop_previous")["detail"]["stopped"], stopped, "{up}");
    assert!(!pid_alive(lost.wrapper) && !pid_alive(lost.listener), "the wrapper and its listener were both stopped");
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    let supervised: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    // The supervisor loses its process; another one of this checkout takes the port.
    unsafe { libc::kill(supervised as i32, libc::SIGKILL) };
    let gone = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() && Instant::now() < gone {
        thread::sleep(Duration::from_millis(10));
    }
    let lost = lost_service(port, &token);
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(step(&up, "ports")["detail"]["stopped"], json!([{ "service": "web", "port": port, "pid": lost.listener, "unrecorded": true }]), "{up}");
    assert!(!pid_alive(lost.wrapper) && !pid_alive(lost.listener));
    fixture.ok(&["down"]);

    // `down` stops it and confirms only once it is gone.
    let lost = lost_service(port, &token);
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    let stopped = json!({ "service": "web", "port": port, "pid": lost.listener, "unrecorded": true });
    assert!(down["data"]["stopped"].as_array().unwrap().contains(&stopped), "{down}");
    assert!(down["data"].get("conflicts").is_none(), "{down}");
    assert!(!pid_alive(lost.wrapper) && !pid_alive(lost.listener));

    // Another checkout's service is another program's: refused and reported, never stopped.
    let lost = lost_service(port, "another-checkout");
    let out = fixture.command(&["up", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "port_conflict");
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["conflicts"][0]["holder"]["pid"], lost.listener, "{down}");
    assert!(pid_alive(lost.wrapper) && pid_alive(lost.listener), "a foreign process was signalled");
}

/// After `compile --reassign-ports` while `web` still runs on `old`: the provider reports the
/// daemon running with its *new* configured port `new`, Pitchfork reports the actual `old`.
/// Stack's supervisor reports where a service really listens, so a generation change needs no
/// stand-in for Pitchfork's active port.
fn report_generation_change(_fixture: &Fixture, _old: u16, _new: u16) {}

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
    assert_eq!(second["data"]["checks"][0]["pid"], first["data"]["checks"][0]["pid"], "the same process");
    assert!(second["data"]["steps"].as_array().unwrap().iter().all(|s| s["status"] == "ok"), "{second}");
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
    assert_eq!(steps, ["compile", "install"], "{result}");
    assert!(!fixture.dir.path().join("state/supervisors").exists(), "no supervisor");
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
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='''printf 'one\\ntwo\\nthree\\n'; exec sleep 600'''\n");
    let out = fixture.command(&["logs", "web", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "logs_failed", "never started: the supervisor has nothing");
    let out = fixture.command(&["logs", "nope", "--json"]).output().unwrap();
    assert_eq!(json_result(&out)["error"]["code"], "unknown_service");

    // Started (it never listens, so `up` fails): what it wrote is kept.
    let _ = fixture.command(&["up", "--timeout", "2s"]).output().unwrap();
    let result = json_result(&fixture.ok(&["logs", "web", "--tail", "2", "--json"]));
    assert_eq!(result["data"]["lines"], json!(["two", "three"]), "{result}");
    assert_eq!(result["data"]["truncated"], false);
    let out = fixture.ok(&["logs", "web"]);
    assert!(String::from_utf8_lossy(&out.stdout).ends_with("one\ntwo\nthree\n"), "{}", String::from_utf8_lossy(&out.stdout));
    let out = fixture.command(&["logs", "web", "--tail", "0"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    // No provider process is asked for logs.
    let log = fs::read_to_string(fixture.dir.path().join("mise.log")).unwrap();
    assert!(!log.contains("daemons"), "{log}");
    fixture.ok(&["down"]);
}

#[test]
fn status_of_a_checkout_never_compiled_here_says_to_start_it_without_asking_the_supervisor() {
    let fixture = Fixture::with_bundle(WEB);
    // A fresh worktree has the committed stack.lock but no generated provider config.
    fs::remove_file(fixture.dir.path().join("app/.config/mise/conf.d/stack.toml")).unwrap();
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "nothing started is not a failed check");
    let result = json_result(&out);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!((&result["data"]["healthy"], &result["data"]["started"]), (&json!(false), &json!(false)), "{result}");
    assert_eq!(result["data"]["checks"][0]["ready"], false);
    assert!(result["data"]["checks"][0]["reason"].as_str().unwrap().contains("stack up"), "{result}");
    let out = fixture.command(&["logs", "web", "--json"]).output().unwrap();
    assert!(json_result(&out)["error"]["hint"].as_str().unwrap().contains("stack up"));
    assert!(!fixture.dir.path().join("state/supervisors").exists(), "no supervisor was started to find that out");
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
fn supervise_never_ready(fixture: &Fixture) {
    fixture.sleepers();
}

/// The pid of the service a [`SLEEPER`] or [`LISTEN`] command started, once it recorded it.
fn service_pid(fixture: &Fixture) -> u32 {
    let path = fixture.dir.path().join("pf-tracked-pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = fs::read_to_string(&path).ok().and_then(|p| p.trim().parse().ok()) {
            return pid;
        }
        assert!(Instant::now() < deadline, "the service never started");
        thread::sleep(Duration::from_millis(10));
    }
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
    supervise_never_ready(&fixture);
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
    let service = service_pid(&fixture);
    assert!(pid_alive(service));
    for record in launch_records(&fixture) {
        assert_eq!(record["launching"], true, "{record}");
        assert_eq!(record["services"]["web"]["pid"], service, "{record}");
    }
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["healthy"], false, "{status}");
    assert!(!fixture.command(&["exec", "--require", "web", "--", "true"]).output().unwrap().status.success());

    // `down` stops it through the supervisor, and nothing is left for GC.
    fixture.ok(&["down"]);
    assert!(!pid_alive(service));
    assert_eq!(json_result(&fixture.ok(&["gc", "--json"]))["data"], json!([]));

    // Once the service can become ready, the same command succeeds.
    let path = fixture.dir.path().join("bundle/bundle.toml");
    fs::write(&path, fs::read_to_string(&path).unwrap().replace(SLEEPER, LISTEN)).unwrap();
    fixture.ok(&["compile"]);
    fixture.ok(&["up", "--timeout", "30s"]);
    fixture.ok(&["exec", "--require", "web", "--", "true"]);
    fixture.ok(&["down"]);
}

#[test]
fn mcp_up_reports_its_timeout_as_an_error_and_down_still_stops_what_launched() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_never_ready(&fixture);
    mcp_up_times_out_and_down_stops(&fixture);
}

/// `stack_up` of `service` (supervised, never ready) with a 2s timeout, then `stack_status` and
/// `stack_down` from the same server: the timeout is a tool error that leaves the launch
/// recorded and unhealthy, and `down` stops it. Returns the timeout's cause. On a loaded
/// machine 1s can pass before the start, when nothing was launched yet.
fn mcp_up_times_out_and_down_stops(fixture: &Fixture) -> Value {
    let results = fixture.mcp(&[("stack_up", json!({ "timeout_secs": 2 })), ("stack_status", json!({})), ("stack_down", json!({}))], &[]);
    assert_eq!(results[0]["isError"], true, "{}", results[0]);
    let error = &results[0]["structuredContent"]["error"];
    assert_eq!(error["code"], "timed_out", "{error}");
    assert_eq!(error["message"], "`stack up` did not finish within 2s");
    assert!(error["hint"].as_str().unwrap().contains("`stack down` stops them"), "{error}");
    assert_cut_short_verifying(error, "web");
    let service = service_pid(fixture);
    let status = &results[1]["structuredContent"]["data"];
    assert_eq!(status["healthy"], false, "{status}");
    assert_eq!(status["session"]["launching"], true, "{status}");
    assert_eq!(status["session"]["services"]["web"]["pid"], service, "{status}");
    assert_eq!(results[2]["isError"], false, "{}", results[2]);
    assert!(!pid_alive(service));
    assert!(fixture.session_paths().iter().all(|p| !p.exists()), "the launch is still recorded after down");
    error["details"][0]["cause"].clone()
}

#[test]
fn startup_waits_for_a_busy_project_only_until_its_timeout() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.web]\nrun='true'\nsetup='touch \"$REVIEW_FIXTURE/client-waiting\"; sleep 30'\n");
    let mut first = fixture.command(&["up", "--timeout", "60s"]).process_group(0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let waiting = fixture.dir.path().join("client-waiting");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiting.exists() {
        assert!(Instant::now() < deadline, "the first start never reached its setup");
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
    fixture.ok(&["down"]);
}

#[test]
fn a_restart_cut_short_keeps_the_session_and_records_the_incomplete_launch() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let up = json_result(&fixture.ok(&["up", "--json"]));
    fs::write(fixture.dir.path().join("slow-start"), "6").unwrap();
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
    assert!(!fixture.dir.path().join("state/supervisors").exists(), "nothing was started");
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
fn a_cold_mise_update_check_cannot_hold_up_the_provider_release_probe() {
    let fixture = Fixture::with_bundle(RUST_MBX);
    // Longer than the probe's whole budget: only skipping the update check gets an answer.
    fs::write(fixture.dir.path().join("version-update-check"), "60").unwrap();
    let start = Instant::now();
    fixture.ok(&["install", "--json"]);
    assert!(start.elapsed() < Duration::from_secs(8), "{:?}", start.elapsed());
}

#[test]
fn a_mise_version_that_misses_its_first_deadline_is_asked_again() {
    let fixture = Fixture::with_bundle(RUST_MBX);
    fs::write(fixture.dir.path().join("version-slow-once"), "12").unwrap();
    let out = fixture.command(&["install", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert!(out.status.success(), "a slow first answer is not `provider_unavailable`: {result}");
    assert!(fixture.dir.path().join("version-slowed").exists(), "the first answer was slow");
    let asked = fs::read_to_string(fixture.dir.path().join("version.log")).unwrap();
    assert!(asked.lines().count() >= 2, "asked once more: {asked}");
}

#[test]
fn doctor_names_another_stack_first_on_path_and_the_release_that_wrote_the_lock() {
    let fixture = Fixture::new();
    let check = |fixture: &Fixture| -> Value {
        let out = fixture.command(&["doctor", "--json"]).output().unwrap();
        let result = json_result(&out);
        let checks = result["data"].as_array().or(result["error"]["details"].as_array()).unwrap().clone();
        checks.into_iter().find(|c| c["name"] == "stack").unwrap_or_else(|| panic!("{result}"))
    };
    let other = fixture.dir.path().join("bin/stack");
    write_exe(&other, "#!/bin/sh\necho 'stack 0.0.9'\n");
    let found = check(&fixture);
    let detail = found["detail"].as_str().unwrap();
    assert!(detail.starts_with(&format!("stack {}", env!("CARGO_PKG_VERSION"))), "{found}");
    assert!(detail.contains("`stack` on PATH is") && detail.contains("(stack 0.0.9), not this one"), "{found}");
    assert_eq!(found["ok"], true, "a notice, not a failure: {found}");
    // This very binary first on PATH: nothing to say about it.
    fs::remove_file(&other).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_stack"), &other).unwrap();
    let lock = fixture.dir.path().join("app/stack.lock");
    let text = fs::read_to_string(&lock).unwrap();
    fs::write(&lock, text.replace(&format!("written_by = \"stack {}\"", env!("CARGO_PKG_VERSION")), "written_by = \"stack 0.0.9\"")).unwrap();
    let found = check(&fixture);
    let detail = found["detail"].as_str().unwrap();
    assert!(!detail.contains("on PATH is"), "{found}");
    assert!(detail.contains("stack.lock was last written by stack 0.0.9"), "{found}");
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
fn a_task_installs_its_missing_pinned_fnox_before_resolving_its_secrets() {
    let fixture = Fixture::new();
    let root = fixture.dir.path();
    let install = root.join("installs/fnox/1.39.0");
    write_exe(&install.join("fnox"), FAKE_FNOX);
    fs::create_dir_all(install.join(".mise-bins")).unwrap();
    std::os::unix::fs::symlink(install.join("fnox"), install.join(".mise-bins/fnox")).unwrap();
    // As mise reports it before and after the install: not installed and off the stack's
    // PATH, then installed with its directory first on PATH.
    installed(&fixture, &[("1.39.0", true)]);
    fs::rename(root.join("ls.json"), root.join("ls-installed.json")).unwrap();
    installed(&fixture, &[("1.39.0", false)]);
    path_first(&fixture, &install.join(".mise-bins"));
    fs::rename(root.join("env.json"), root.join("env-installed.json")).unwrap();
    fs::write(root.join("env.json"), json!({ "PATH": format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap()) }).to_string()).unwrap();
    fs::write(
        root.join("app/stack.toml"),
        "[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n[tasks.sign]\nrun = 'x'\nsecrets = ['DEPLOY_KEY']\n",
    )
    .unwrap();
    fixture.ok(&["compile"]);
    for route in ["cli", "mcp"] {
        let _ = fs::remove_file(root.join("installed"));
        let envelope = match route {
            "cli" => json_run(&fixture, &["run", "sign"]).0,
            _ => fixture.mcp(&[("stack_run", json!({ "task": "sign" }))], &[])[0]["structuredContent"].clone(),
        };
        assert_eq!(envelope["ok"], true, "{route}: {envelope}");
        assert_eq!(envelope["data"]["exit_code"], 0, "{route}: {envelope}");
        assert_eq!(envelope["data"]["secrets"], json!(["DEPLOY_KEY"]), "{route}: {envelope}");
        assert!(fs::read_to_string(root.join("installed")).unwrap().contains("fnox@1.39.0"), "{route}: fnox was installed first");
        assert_no_leak(&envelope.to_string(), route);
    }
    assert!(fnox_log(&fixture).iter().any(|l| l.contains("--keys")), "the pinned fnox resolved the grant");
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
    // A timeout bounds planning too, so on a slow machine a run may end before reaching mise.
    let reached = |fixture: &Fixture| fs::read_to_string(fixture.dir.path().join("run.log")).map_or(0, |log| log.lines().count());
    let timed_out = reached(&fixture);
    // Planning fails after the copy was made: fnox cannot answer for the grant.
    fs::write(fixture.dir.path().join("fnox-keys-mode"), "exit").unwrap();
    let (envelope, text, _) = json_run(&fixture, &["run", "showenv"]);
    assert_eq!(envelope["ok"], false, "{text}");
    assert_no_leak(&text, "failed plan");
    let results = fixture.mcp(&[("stack_run", json!({ "task": "showenv" }))], &[]);
    assert_eq!(results[0]["structuredContent"]["ok"], false, "{}", results[0]);
    // The failed plans never reached mise.
    assert_eq!(reached(&fixture), timed_out);
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
    // A template is declared; a literal would be kept as written (and need no value).
    let (fixture, _) = artifact_fixture("[env]\nTASK_ENV='{{ \"from-project\" }}'\n[tasks.q]\nrun='echo $TASK_ENV'\n");
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

// ---- machine-wide commands -------------------------------------------------------------------

/// `web`, a Postgres preset, started by Stack's supervisor with the data directory mise's preset
/// kept for this checkout (see `legacy_data`), holding `bytes` bytes; `up` records it (marker,
/// machine data record).
fn up_with_data(fixture: &Fixture, up_args: &[&str], bytes: usize) -> (u16, std::path::PathBuf) {
    let port = fixture.postgres_web();
    let data = fixture.legacy_data("0123");
    fs::write(data.join("rows"), vec![b'x'; bytes]).unwrap();
    fixture.ok(&[&["up"], up_args].concat());
    (port, data)
}

#[test]
fn ps_lists_every_checkout_without_a_lock_or_a_stack_toml() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let app = dir.join("app").canonicalize().unwrap();
    let (port, data) = up_with_data(&fixture, &["--ttl", "30m"], 8192);
    // Another checkout, deleted while its session is still recorded.
    let gone = dir.canonicalize().unwrap().join("deleted");
    let record = json!({ "id": "gone12345678", "project": gone, "lock_digest": "", "started_at": 1, "services": { "web": { "port": 1, "identity": "liveness", "verified_at": 1 } } });
    fs::write(dir.join(format!("state/sessions/{}.json", stack::state::project_key(&gone))), record.to_string()).unwrap();

    let ps = |args: &[&str]| -> Value {
        let out = fixture.command_at(dir, &[&["ps", "--json"], args].concat()).output().unwrap();
        let result = json_result(&out);
        assert!(out.status.success(), "{result}");
        result["data"].clone()
    };
    let row = |report: &Value, path: &Path| -> Value {
        report["checkouts"].as_array().unwrap().iter().find(|c| Path::new(c["path"].as_str().unwrap()) == path).cloned().unwrap_or_else(|| panic!("{path:?} in {report}"))
    };
    // From a directory without stack.toml, while `up` would hold the project's lock.
    let held = stack::state::project_lock(&dir.join("state"), &app).unwrap();
    let started = Instant::now();
    let report = ps(&[]);
    assert!(started.elapsed() < Duration::from_secs(10));
    drop(held);
    assert!(report["checks"].as_str().unwrap().starts_with("liveness only"), "{report}");
    let a = row(&report, &app);
    assert_eq!((a["dir"].as_str(), a["state"].as_str(), a["reclaimable"].as_bool()), (Some("present"), Some("running"), Some(false)), "{a}");
    assert_eq!(a["lease"]["kind"], "ttl", "{a}");
    assert_eq!(a["lease"]["expired"], false, "{a}");
    let web = &a["services"][0];
    assert_eq!((web["name"].as_str(), web["port"].as_u64()), (Some("web"), Some(u64::from(port))), "{web}");
    assert_eq!(web["port_accepting"], true, "{web}");
    assert_eq!(web["data_dir"].as_str(), Some(data.to_str().unwrap()), "{web}");
    assert!(web["data_bytes"].as_u64().unwrap() >= 8192, "{web}");
    let g = row(&report, &gone);
    assert_eq!((g["dir"].as_str(), g["state"].as_str(), g["reclaimable"].as_bool()), (Some("missing"), Some("stopped"), Some(false)), "{g}");
    assert_eq!(g["lease"]["kind"], "none");
    assert_eq!(report["totals"]["checkouts"], 2, "{report}");
    assert_eq!(report["totals"]["running"], 1, "{report}");
    assert_eq!(report["totals"]["reclaimable_bytes"], 0, "{report}");
    assert_eq!(row(&ps(&["--no-size"]), &app)["services"][0]["data_bytes"], Value::Null);
    let human = String::from_utf8_lossy(&fixture.command_at(dir, &["ps"]).output().unwrap().stdout).into_owned();
    assert!(human.contains("running  session") && human.contains("directory missing") && human.contains("liveness only"), "{human}");
    let mcp = fixture.mcp(&[("stack_ps", json!({ "no_size": true }))], &[]);
    assert_eq!(mcp[0]["structuredContent"]["data"]["totals"]["checkouts"], 2, "{}", mcp[0]);

    // Stopped and removed: what `gc` would reclaim, its data included.
    fixture.ok(&["down"]);
    fs::remove_dir_all(dir.join("app")).unwrap();
    let report = ps(&[]);
    let a = row(&report, &app);
    assert_eq!((a["dir"].as_str(), a["state"].as_str(), a["reclaimable"].as_bool()), (Some("missing"), Some("reserved"), Some(true)), "{a}");
    assert_eq!(a["services"][0]["data_dir"].as_str(), Some(data.to_str().unwrap()), "from the data record: {a}");
    assert!(report["totals"]["reclaimable_bytes"].as_u64().unwrap() >= 8192, "{report}");
    let human = String::from_utf8_lossy(&fixture.command_at(dir, &["ps"]).output().unwrap().stdout).into_owned();
    assert!(human.contains("reclaimable by `stack gc`"), "{human}");
}

#[test]
fn mv_carries_a_moved_checkouts_ports_tokens_and_data_to_its_new_path() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let (port, data) = up_with_data(&fixture, &[], 8192);
    fixture.ok(&["down"]);
    let app = dir.join("app").canonicalize().unwrap();
    let supervisor = dir.join("state/supervisors").join(stack::state::project_key(&app));
    assert!(supervisor.join("state.json").exists(), "the stopped supervisor's record stays until the checkout moves");
    // An identity token, as a compile gives a service with an identity probe.
    let token = "stack-0123456789abcdef0123456789abcdef";
    fs::write(dir.join("state/identities.json"), json!({ "tokens": [{ "project": app, "service": "web", "token": token }] }).to_string()).unwrap();
    let tokens = || -> Value { serde_json::from_slice(&fs::read(dir.join("state/identities.json")).unwrap()).unwrap() };
    let moved = dir.canonicalize().unwrap().join("moved");
    fs::rename(&app, &moved).unwrap();
    // Where Stack keeps a checkout's data at the new path.
    let new_data = dir.join("state/data").join(stack::state::project_key(&moved)).join("web");

    // Relative paths, from the parent directory.
    let out = fixture.command_at(dir, &["mv", "app", "moved", "--json"]).current_dir(dir).output().unwrap();
    let result = json_result(&out);
    assert!(out.status.success(), "{result}");
    let m = &result["data"];
    assert_eq!((m["from"].as_str(), m["to"].as_str()), (app.to_str(), moved.to_str()), "{result}");
    assert_eq!((m["ports"].as_u64(), m["identity_tokens"].as_u64()), (Some(1), Some(1)), "{result}");
    assert_eq!(m["data"][0]["to"].as_str(), new_data.to_str(), "{result}");
    assert_eq!(fs::read(new_data.join("rows")).unwrap().len(), 8192);
    assert!(!data.exists() && !data.with_file_name("web.stack-checkout").exists(), "the old directory and marker are gone");
    assert!(!supervisor.exists(), "the supervisor directory of the old path is forgotten");
    // Marked and recorded as the checkout's under the token its new path gives it.
    let new_token = stack::checkout::token(&moved).unwrap();
    let marker: Value = serde_json::from_slice(&fs::read(new_data.with_file_name("web.stack-checkout")).unwrap()).unwrap();
    assert_eq!(marker["checkout"], new_token.as_str());
    let ledger: Value = serde_json::from_slice(&fs::read(dir.join("state/data.json")).unwrap()).unwrap();
    let entries = ledger["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{ledger}");
    assert_eq!(entries[0]["project"].as_str(), moved.to_str(), "{ledger}");
    assert_eq!(entries[0]["checkout"], new_token.as_str(), "{ledger}");
    assert_eq!(entries[0]["location"], json!(new_data.canonicalize().unwrap()), "{ledger}");
    assert!(entries[0]["identity"].is_array(), "{ledger}");
    assert_eq!(tokens()["tokens"][0]["token"], token);
    assert_eq!(tokens()["tokens"][0]["project"].as_str(), moved.to_str());
    // The checkout compiles at its new path with its old port, and gc leaves it alone.
    let inspect = json_result(&fixture.command_at(&moved, &["inspect", "--json"]).output().unwrap());
    assert_eq!(inspect["data"]["ports"]["web"].as_u64(), Some(u64::from(port)), "{inspect}");
    let gc = json_result(&fixture.command_at(dir, &["gc", "--reclaim-data", "--json"]).output().unwrap());
    assert_eq!(gc["data"], json!([]), "{gc}");
    assert!(new_data.join("rows").exists());
}

#[test]
fn mv_refuses_what_it_cannot_do_safely_and_changes_nothing() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let app = dir.join("app").canonicalize().unwrap();
    let port = assigned_port(&fixture, "web");
    let ports = || fs::read_to_string(dir.join("state/ports.json")).unwrap();
    let mv = |args: &[&str]| json_result(&fixture.command_at(dir, &[&["mv"], args, &["--json"]].concat()).current_dir(dir).output().unwrap());

    // The old path still exists: a copy, not a move.
    fs::create_dir(dir.join("copy")).unwrap();
    assert_eq!(mv(&["app", "copy"])["error"]["code"], "mv_source_exists");
    let r = mv(&["never-there", "copy"]);
    assert_eq!(r["error"]["code"], "mv_nothing_recorded", "{r}");
    assert!(r["error"]["hint"].as_str().unwrap().contains("stack ps"), "{r}");
    assert_eq!(mv(&["app", "nowhere"])["error"]["code"], "mv_target_missing");
    // Over MCP; a missing argument is a usage error.
    let copy = dir.canonicalize().unwrap().join("copy");
    let mcp = fixture.mcp(&[("stack_mv", json!({ "from": app, "to": copy })), ("stack_mv", json!({ "from": app }))], &[]);
    assert_eq!(mcp[0]["structuredContent"]["error"]["code"], "mv_source_exists", "{}", mcp[0]);
    assert_eq!(mcp[1]["structuredContent"]["error"]["code"], "usage", "{}", mcp[1]);

    // Services may still run: a live recorded pid.
    let mut live = Command::new("sleep").arg("60").spawn().unwrap();
    let index = dir.join(format!("state/sessions/{}.json", stack::state::project_key(&app)));
    fs::create_dir_all(index.parent().unwrap()).unwrap();
    let record = json!({ "id": "sess12345678", "project": app, "lock_digest": "", "started_at": 1, "services": { "web": { "port": port, "pid": live.id(), "identity": "liveness", "verified_at": 1 } } });
    fs::write(&index, record.to_string()).unwrap();
    let moved = dir.canonicalize().unwrap().join("moved");
    fs::rename(&app, &moved).unwrap();
    let r = mv(&["app", "moved"]);
    assert_eq!(r["error"]["code"], "mv_services_running", "{r}");
    assert!(r["error"]["hint"].as_str().unwrap().contains("down"), "{r}");
    assert!(ports().contains(app.to_str().unwrap()) && index.exists(), "nothing changed");

    // The moved checkout points at `stack mv` when the record that came with it is in the way.
    fs::create_dir_all(moved.join(".stack")).unwrap();
    fs::rename(&index, moved.join(".stack/session.json")).unwrap();
    let r = json_result(&fixture.command_at(&moved, &["status", "--json"]).output().unwrap());
    assert_eq!(r["error"]["code"], "session_conflict", "{r}");
    assert!(r["error"]["hint"].as_str().unwrap().contains(&format!("stack mv {} {}", app.display(), moved.display())), "{r}");
    let _ = live.kill();
    let _ = live.wait();

    // The new path has a session of its own.
    let other = dir.canonicalize().unwrap().join("other");
    fs::create_dir(&other).unwrap();
    let theirs = dir.join(format!("state/sessions/{}.json", stack::state::project_key(&other)));
    fs::write(&theirs, json!({ "id": "other1234567", "project": other, "lock_digest": "", "started_at": 1, "services": {} }).to_string()).unwrap();
    assert_eq!(mv(&["app", "other"])["error"]["code"], "mv_target_has_state");

    // Data recorded for the checkout that cannot be confirmed to be its own refuses the move.
    let data = dir.join("mise-state/daemons/0123/data/web");
    fs::create_dir_all(&data).unwrap();
    let token = stack::checkout::token_as(&app, &moved).unwrap();
    let entry = json!({ "project": app, "service": "web", "dir": data, "checkout": token });
    fs::write(dir.join("state/data.json"), json!({ "entries": [entry] }).to_string()).unwrap();
    let r = mv(&["app", "moved"]);
    assert_eq!(r["error"]["code"], "mv_data_unmovable", "{r}");
    assert!(ports().contains(app.to_str().unwrap()), "nothing changed");
    fs::remove_file(dir.join("state/data.json")).unwrap();

    // With the service stopped, the record that came with the directory is enough to move it.
    let r = mv(&["app", "moved"]);
    assert_eq!(r["data"]["session"], "sess12345678", "{r}");
    assert!(dir.join(format!("state/sessions/{}.json", stack::state::project_key(&moved))).exists());
    assert!(ports().contains(moved.to_str().unwrap()) && !ports().contains(&format!("{}\"", app.display())), "{}", ports());
}

#[test]
fn down_purge_removes_the_data_up_recorded_only_where_gc_would_and_releases_ports() {
    let fixture = Fixture::with_bundle(WEB);
    let dir = fixture.dir.path();
    let (_port, data) = up_with_data(&fixture, &[], 8192);
    let marker = data.with_file_name("web.stack-checkout");
    fs::write(data.with_file_name("unsaved.txt"), "keep me").unwrap();
    let purge = |args: &[&str]| -> Value {
        let out = fixture.command(&[&["down", "--purge", "--json"], args].concat()).output().unwrap();
        let result = json_result(&out);
        assert!(out.status.success(), "{result}");
        assert_eq!(result["data"]["confirmed"], true, "{result}");
        result["data"]["purge"].clone()
    };

    // The supervisor the record names does not report the service stopped: the data is kept.
    // (Stack's supervisor reports nothing running once `down` confirmed the stop; data whose
    // recorded supervisor reports its service running is kept: see
    // `gc_releases_a_removed_checkouts_ports_and_removes_its_data_only_when_asked_and_safe`.)

    // Confirmed: removed with its marker, and nothing beside it; the record is settled.
    let p = purge(&[]);
    assert_eq!(p["complete"], true, "{p}");
    assert_eq!(p["data"][0]["kept"], false, "{p}");
    assert!(p["data"][0]["outcome"].as_str().unwrap().starts_with(&format!("data in {} removed", data.display())), "{p}");
    assert!(!data.exists() && !marker.exists());
    assert_eq!(fs::read_to_string(data.with_file_name("unsaved.txt")).unwrap(), "keep me");
    let ledger: Value = serde_json::from_slice(&fs::read(dir.join("state/data.json")).unwrap()).unwrap();
    assert_eq!(ledger["entries"], json!([]), "{ledger}");
    assert!(!fs::read_to_string(dir.join("state/ports.json")).unwrap().contains("\"web\""));

    // Nothing left: purging again is safe, over MCP too.
    let mcp = fixture.mcp(&[("stack_down", json!({ "purge": true }))], &[]);
    let p = &mcp[0]["structuredContent"]["data"]["purge"];
    assert_eq!((p["complete"].clone(), p["data"].clone()), (json!(true), json!([])), "{}", mcp[0]);
}

#[test]
fn exec_reports_truncation_and_full_size_of_each_stream() {
    let fixture = Fixture::new();
    let script = "head -c 70000 /dev/zero | tr '\\0' a; printf 0123456789 >&2";
    let result = json_result(&fixture.ok(&["--json", "exec", "--", "sh", "-c", script]));
    let data = &result["data"];
    assert_eq!((data["stdout_truncated"].as_bool(), data["stderr_truncated"].as_bool()), (Some(true), Some(false)), "{data}");
    assert_eq!((data["stdout_bytes"].as_u64(), data["stderr_bytes"].as_u64()), (Some(70000), Some(10)));
    let results = fixture.mcp(&[("stack_exec", json!({ "command": ["sh", "-c", "head -c 70000 /dev/zero >&2"] }))], &[]);
    let data = &results[0]["structuredContent"]["data"];
    assert_eq!((data["stdout_truncated"].as_bool(), data["stderr_truncated"].as_bool()), (Some(false), Some(true)), "{data}");
    assert_eq!(data["stderr_bytes"], 70000);
}

#[test]
fn a_service_that_does_not_verify_has_a_stable_reason_code() {
    let fixture = Fixture::with_bundle(WEB);
    let status = json_result(&fixture.command(&["--json", "status"]).output().unwrap());
    let web = &status["data"]["checks"][0];
    assert_eq!((web["service"].as_str(), web["ready"].as_bool()), (Some("web"), Some(false)), "{status}");
    assert_eq!(web["reason_code"], "not_launched", "{status}");
    assert!(web["reason"].is_string(), "{status}");
    let results = fixture.mcp(&[("stack_status", json!({}))], &[]);
    assert_eq!(results[0]["structuredContent"]["data"]["checks"][0]["reason_code"], "not_launched", "{}", results[0]);
}

/// A checkout never started, or stopped by a confirmed `down`, is not a failed check: `status`
/// exits 0, says to run `up`, and `started: false` (with `healthy: false`: nothing was verified)
/// tells it apart from a healthy one. A started checkout with a service that is not ready, or
/// one whose service still runs without a session, exits 1 as before.
#[test]
fn status_of_a_checkout_with_nothing_started_exits_0_and_says_so() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let not_started = |when: &str| {
        let out = fixture.command(&["status"]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{when}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "not started; run `stack up`\n", "{when}");
        let out = fixture.command(&["status", "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{when}");
        let status = json_result(&out);
        let data = &status["data"];
        assert_eq!((&status["passed"], &data["started"], &data["healthy"]), (&json!(true), &json!(false), &json!(false)), "{when}: {status}");
        assert_eq!(data["checks"][0]["reason_code"], "not_launched", "{when}: {status}");
        let results = fixture.mcp(&[("stack_status", json!({}))], &[]);
        let content = &results[0]["structuredContent"];
        assert_eq!((&content["passed"], &content["data"]["started"]), (&json!(true), &json!(false)), "{when}: {content}");
    };
    not_started("never started");

    fixture.ok(&["up"]);
    let status = json_result(&fixture.ok(&["status", "--json"]));
    assert_eq!((&status["passed"], &status["data"]["started"], &status["data"]["healthy"]), (&json!(true), &json!(true), &json!(true)), "{status}");

    // A service of the session stopped: started, not healthy, exit 1.
    let pid: i32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid as u32) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let out = fixture.command(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.starts_with("session ") && text.contains("NOT READY") && !text.contains("not started"), "{text}");
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let status = json_result(&out);
    assert_eq!((&status["passed"], &status["data"]["started"], &status["data"]["healthy"]), (&json!(false), &json!(true), &json!(false)), "{status}");
    let results = fixture.mcp(&[("stack_status", json!({}))], &[]);
    let content = &results[0]["structuredContent"];
    assert_eq!((&content["ok"], &content["passed"], &content["data"]["started"]), (&json!(true), &json!(false), &json!(true)), "{content}");

    // After a confirmed `down` there is no session again, and nothing runs: as never started.
    fixture.ok(&["down"]);
    not_started("after a confirmed down");
}

/// A service the supervisor still runs for this checkout is something started, even once its
/// session record is gone: `status` does not call that "not started".
#[test]
fn status_without_a_session_but_with_a_running_service_is_not_not_started() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    for path in fixture.session_paths() {
        fs::remove_file(path).unwrap();
    }
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status = json_result(&out);
    assert_eq!(out.status.code(), Some(1), "{status}");
    assert!(status["data"]["session"].is_null(), "{status}");
    assert_eq!((&status["passed"], &status["data"]["started"], &status["data"]["healthy"]), (&json!(false), &json!(true), &json!(false)), "{status}");
    let text = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
    assert!(text.starts_with("no session") && text.contains("NOT READY"), "{text}");
    fixture.ok(&["down"]);
}

/// Not started, `status` still shows what would make `up` fail: another program on a service's
/// port, with the way out, beside the "not started" line, and still exits 0.
#[test]
fn status_of_a_checkout_not_started_still_names_a_port_another_program_holds() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    let _held = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let out = fixture.command(&["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("not started; run `stack up`"), "{text}");
    let web = lines.next().unwrap_or_else(|| panic!("{text}"));
    assert!(web.starts_with("web ") && web.contains(&format!("is now listening on its port {port}")) && web.contains("--reassign-ports"), "{text}");
    assert!(!web.contains("no launch record"), "only what is more than not started: {text}");
}

/// With both session copies gone, a service still running under this checkout's supervisor is
/// found by the look `down` relies on even when the cheap evidence misses it: (a) the manifest
/// renamed the service, so no check names it; (b) the supervisor's record cannot be read, so it
/// lists nothing. `status` is then `started`, exits 1, and says what it found.
#[test]
fn status_without_a_session_finds_a_service_the_checks_miss() {
    for case in ["renamed", "corrupt"] {
        let fixture = Fixture::with_bundle(WEB);
        supervise_listener(&fixture, assigned_port(&fixture, "web"));
        fixture.ok(&["up"]);
        let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
        for path in fixture.session_paths() {
            fs::remove_file(path).unwrap();
        }
        let bundle = fixture.dir.path().join("bundle/bundle.toml");
        let original = fs::read_to_string(&bundle).unwrap();
        let state = fs::read_dir(fixture.dir.path().join("state/supervisors")).unwrap().next().unwrap().unwrap().path().join("state.json");
        let saved = fs::read(&state).unwrap();
        if case == "renamed" {
            fs::write(&bundle, original.replace("[services.web]", "[services.api]")).unwrap();
            fixture.ok(&["compile"]);
        } else {
            fs::write(&state, "{ not json").unwrap();
        }
        let out = fixture.command(&["status", "--json"]).output().unwrap();
        let status = json_result(&out);
        assert!(pid_alive(pid), "{case}: the service runs");
        assert_eq!(out.status.code(), Some(1), "{case}: {status}");
        let data = &status["data"];
        assert_eq!((&status["passed"], &data["started"], &data["healthy"], &data["session"]), (&json!(false), &json!(true), &json!(false), &Value::Null), "{case}: {status}");
        let found = data["found"].to_string();
        match case {
            "renamed" => assert!(found.contains(&format!("service web (pid {pid})")), "{case}: {status}"),
            _ => assert!(found.contains("cannot tell what this checkout's supervisor runs"), "{case}: {status}"),
        }
        let text = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
        assert!(text.starts_with("no session") && text.contains("found") && !text.contains("not started"), "{case}: {text}");
        fs::write(&state, &saved).unwrap();
        fs::write(&bundle, &original).unwrap();
        fixture.ok(&["compile"]);
        fixture.ok(&["down"]);
        assert!(!pid_alive(pid), "{case}");
    }
}

/// A service left running by a checkout that was moved away, with its session index gone, and a
/// fresh checkout (another directory, another token) at the same path: the supervisor record at
/// this path is the other checkout's. `status` here does not call it this checkout's: not
/// started, exit 0, with `other_checkout` naming what runs; `up` here stops it and starts this
/// checkout's own.
#[test]
fn status_does_not_take_another_checkouts_supervisor_at_this_path_for_its_own() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let pid: u32 = fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap();
    for path in fixture.session_paths().into_iter().skip(1) {
        fs::remove_file(path).unwrap();
    }
    let app = fixture.dir.path().join("app");
    let old = fixture.dir.path().join("app-old");
    fs::rename(&app, &old).unwrap();
    fs::create_dir(&app).unwrap();
    fs::copy(old.join("stack.toml"), app.join("stack.toml")).unwrap();
    fs::copy(old.join("stack.lock"), app.join("stack.lock")).unwrap();
    fixture.ok(&["compile"]);
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status = json_result(&out);
    let data = &status["data"];
    assert_eq!(out.status.code(), Some(0), "{status}");
    assert_eq!((&data["started"], &status["passed"], data.get("found")), (&json!(false), &json!(true), None), "{status}");
    let other = data["other_checkout"].to_string();
    assert!(other.contains(&format!("service web (pid {pid}) runs for that other checkout")) && !other.contains("this checkout's supervisor"), "{status}");
    let text = String::from_utf8_lossy(&fixture.command(&["status"]).output().unwrap().stdout).into_owned();
    assert!(text.starts_with("not started; run `stack up`\n") && text.contains("other        the supervisor at this path"), "{text}");
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(up["data"]["steps"].as_array().unwrap().iter().any(|s| s["step"] == "stop_previous"), "{up}");
    assert!(!pid_alive(pid), "up stops the other checkout's service first");
    fixture.ok(&["down"]);
}

/// The marker a `down` that did not confirm leaves: `status` without a session is started and
/// says so; `up` starts anyway and warns that what that `down` reported may still run; `up`
/// leaves the marker, and the `down` that confirms removes it.
#[test]
fn an_unconfirmed_down_marker_keeps_status_started_and_up_warns() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let identity = stack::checkout::identity(&fixture.dir.path().join("app")).unwrap();
    let marker = fixture.dir.path().join("state/unconfirmed").join(format!("{identity}.json"));
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, json!({ "at_ms": 0, "code": "stop_unconfirmed", "message": "services did not stop" }).to_string()).unwrap();
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status = json_result(&out);
    assert_eq!((out.status.code(), &status["data"]["started"]), (Some(1), &json!(true)), "{status}");
    assert!(status["data"]["found"][0].as_str().unwrap().contains("(stop_unconfirmed: services did not stop)"), "{status}");
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(up["data"]["warnings"].to_string().contains("the last `stack down` here was not confirmed"), "{up}");
    assert!(marker.exists(), "up leaves it to the next down");
    fixture.ok(&["down"]);
    assert!(!marker.exists(), "a confirmed down removes it");
    // One that cannot be read proves nothing either.
    fs::write(&marker, "{ not json").unwrap();
    let out = fixture.command(&["status", "--json"]).output().unwrap();
    let status = json_result(&out);
    assert_eq!((out.status.code(), &status["data"]["started"]), (Some(1), &json!(true)), "{status}");
    assert!(status["data"]["found"][0].as_str().unwrap().contains("cannot tell whether the last `stack down` here was confirmed"), "{status}");
    fixture.ok(&["down"]);
}

/// Every envelope says what ran, with which Stack, in which schema, and how long it took.
fn assert_self_described(envelope: &Value, command: &str) {
    assert_eq!(envelope["command"], command, "{envelope}");
    assert_eq!(envelope["schema"], 1, "{envelope}");
    assert_eq!(envelope["stack_version"], env!("CARGO_PKG_VERSION"), "{envelope}");
    assert!(envelope["duration_ms"].is_u64(), "{envelope}");
    assert!(envelope["passed"].is_boolean(), "{envelope}");
}

#[test]
fn every_envelope_describes_itself_and_says_whether_it_passed() {
    let fixture = Fixture::new();
    let out = fixture.command(&["--json", "exec", "--", "sh", "-c", "exit 7"]).output().unwrap();
    assert_eq!(out.status.code(), Some(7));
    let result = json_result(&out);
    assert_self_described(&result, "exec");
    assert_eq!((result["ok"].as_bool(), result["passed"].as_bool()), (Some(true), Some(false)), "{result}");
    let result = json_result(&fixture.ok(&["--json", "exec", "--", "true"]));
    assert_eq!((result["ok"].as_bool(), result["passed"].as_bool()), (Some(true), Some(true)), "{result}");

    // Stack's own failure, and an argument clap refused, name the command too.
    let out = fixture.command(&["--json", "restart"]).output().unwrap();
    assert_eq!(out.status.code(), Some(70), "a Stack error exits 70");
    let result = json_result(&out);
    assert_self_described(&result, "restart");
    assert_eq!((result["ok"].as_bool(), result["passed"].as_bool()), (Some(false), Some(false)), "{result}");
    let out = fixture.command(&["--json", "logs", "--tail", "many"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let result = json_result(&out);
    assert_self_described(&result, "logs");
    assert_eq!(result["error"]["code"], "usage", "{result}");

    // A status of a checkout with nothing started passed (an unhealthy started one does not:
    // `status_of_a_checkout_with_nothing_started_exits_0_and_says_so`).
    let fixture = Fixture::with_bundle(WEB);
    let result = json_result(&fixture.command(&["--json", "status"]).output().unwrap());
    assert_self_described(&result, "status");
    assert_eq!((result["ok"].as_bool(), result["passed"].as_bool()), (Some(true), Some(true)), "{result}");
    let results = fixture.mcp(&[("stack_status", json!({})), ("stack_inspect", json!({}))], &[]);
    assert_self_described(&results[0]["structuredContent"], "status");
    assert_eq!(results[0]["structuredContent"]["passed"], true);
    assert_self_described(&results[1]["structuredContent"], "inspect");
    assert_eq!(results[1]["structuredContent"]["passed"], true);
}

#[test]
fn every_error_code_is_registered_with_a_hint_a_retry_flag_and_next_steps() {
    let fixture = Fixture::new();
    let listed = json_result(&fixture.ok(&["--json", "errors"]));
    let codes = listed["data"].as_array().unwrap();
    let no_session = codes.iter().find(|i| i["code"] == "no_session").unwrap();
    assert_eq!(no_session["retryable"], false, "{no_session}");
    assert!(no_session["hint"].as_str().is_some_and(|h| h.contains("stack up")), "{no_session}");
    assert_eq!(fixture.ok(&["errors", "--markdown"]).stdout, fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/user/errors.md")).unwrap());

    // A failure names the operation that fixes it, in this checkout, for the CLI and for MCP.
    let failed = json_result(&fixture.command(&["--json", "restart"]).output().unwrap());
    let app = fixture.dir.path().join("app").canonicalize().unwrap().display().to_string();
    assert_eq!(failed["error"]["code"], "no_session", "{failed}");
    assert_eq!(failed["error"]["retryable"], false, "{failed}");
    assert_eq!(failed["error"]["next"], json!([{ "command": ["stack", "-C", app, "up"], "tool": "stack_up", "args": { "dir": app } }]), "{failed}");
    let results = fixture.mcp(&[("stack_restart", json!({})), ("stack_errors", json!({}))], &[]);
    assert_eq!(results[0]["structuredContent"]["error"]["next"], failed["error"]["next"], "{}", results[0]);
    assert_eq!(results[1]["structuredContent"]["data"], listed["data"], "{}", results[1]);
}

// ---- Stack's own supervisor -----------------------------------------------------------------

/// The supervisor of the checkout killed outright: its service runs on unsupervised, so it is
/// no longer verified (its endpoints withheld); `down` stops it from the recorded pid, start
/// time and group, and `up` replaces it under a new supervisor.
#[test]
fn a_killed_supervisor_leaves_nothing_down_cannot_stop_and_up_replaces_it() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let service = service_pid(&fixture);
    let supervisor = fixture.supervisor_state()["supervisor"]["pid"].as_u64().unwrap() as u32;
    unsafe { libc::kill(supervisor as i32, libc::SIGKILL) };
    let gone = Instant::now() + Duration::from_secs(5);
    while pid_alive(supervisor) && Instant::now() < gone {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(200));
    if pid_alive(service) {
        // macOS: nothing stops it on its own.
        let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
        assert_eq!(status["data"]["checks"][0]["ready"], false, "{status}");
        assert!(status["data"]["checks"][0]["reason"].as_str().unwrap().contains("orphaned"), "{status}");
        let out = fixture.ok(&["exec", "--", "sh", "-c", PRINT_ENDPOINTS]);
        assert_eq!(printed(&String::from_utf8_lossy(&out.stdout))["STACK_UNVERIFIED"], "web");
    }
    // `up` stops the orphan (if any) and starts the service again under a new supervisor.
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    assert!(!pid_alive(service));
    let replacement = fixture.session()["services"]["web"]["pid"].as_u64().unwrap() as u32;
    assert_ne!(replacement, service);
    // Killed again, `down` alone stops what it left and confirms.
    let supervisor = fixture.supervisor_state()["supervisor"]["pid"].as_u64().unwrap() as u32;
    unsafe { libc::kill(supervisor as i32, libc::SIGKILL) };
    thread::sleep(Duration::from_millis(200));
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(replacement));
    assert!(fixture.supervisor_state()["supervisor"].is_null());
}

/// Services start in their dependency order through Stack's supervisor: one starts only once
/// what it depends on verified and is ready, and services of one wave start together.
#[test]
fn up_starts_each_wave_once_the_one_before_is_ready_and_a_wave_at_once() {
    let slow = format!("sleep 1; {LISTEN}");
    let check = r#"if "$REVIEW_PYTHON" -c 'import socket,sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), 1)' "$DB_PORT"; then echo up; else echo down; fi >"$REVIEW_FIXTURE/db-at-api-start"; "#;
    let fixture = Fixture::with_bundle(&format!(
        "[bundle]\nname='test'\n[services.db]\nrun='''{slow}'''\n[services.cache]\nrun='''{slow}'''\n[services.api]\nrun='''{check}{LISTEN}'''\ndepends_on=['db', 'cache']\n"
    ));
    let db = assigned_port(&fixture, "db");
    fixture.set_env(&[("DB_PORT", db.to_string())]);
    let started = Instant::now();
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let took = started.elapsed();
    assert!(up["data"]["checks"].as_array().unwrap().iter().all(|c| c["ready"] == true), "{up}");
    assert_eq!(fs::read_to_string(fixture.dir.path().join("db-at-api-start")).unwrap().trim(), "up", "api started before db was ready");
    // db and cache took a second each, together.
    assert!(took < Duration::from_millis(1900), "the first wave started one at a time: {took:?}");
    fixture.ok(&["down"]);
}

/// A session Stack 0.1.23 started under Pitchfork: `status` asks Pitchfork about it, `restart`
/// refuses, and `up` stops it through Pitchfork and starts the service under Stack's own
/// supervisor.
#[test]
fn a_session_running_under_pitchfork_is_asked_there_and_moved_by_up() {
    let fixture = Fixture::with_bundle(WEB);
    let running = fixture.start_web(&[]);
    let pid = running.service.0;
    fixture.supervisor_tracks(pid, running.port);
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!(status["data"]["checks"][0]["pid"], pid, "{status}");
    assert!(fixture.pitchfork_log().contains(&format!("status --json {WEB_ID}")), "{}", fixture.pitchfork_log());
    let out = fixture.command(&["restart", "--json"]).output().unwrap();
    let error = json_result(&out)["error"].clone();
    assert_eq!(error["code"], "session_stale", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("stack up"), "{error}");
    drop(running.listener);
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(fixture.pitchfork_log().contains(&format!("stop -- {WEB_ID}")), "stopped through Pitchfork: {}", fixture.pitchfork_log());
    assert!(!pid_alive(pid));
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    let record = fixture.session();
    assert_eq!(record["provider"]["supervisor"], "stack", "{record}");
    assert_eq!(record["provider"]["pitchfork"], "", "{record}");
    fixture.ok(&["down"]);
}

// ---- review fixes ---------------------------------------------------------------------------

impl Fixture {
    fn remove_session_records(&self) {
        let _ = fs::remove_file(self.dir.path().join("app/.stack/session.json"));
        if let Ok(entries) = fs::read_dir(self.dir.path().join("state/sessions")) {
            for e in entries.flatten() {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    fn supervisor_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("state/supervisors").join(stack::state::project_key(&self.dir.path().join("app").canonicalize().unwrap()))
    }

    fn kill_supervisor(&self) -> u32 {
        let pid = self.supervisor_state()["supervisor"]["pid"].as_u64().unwrap() as u32;
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        let gone = Instant::now() + Duration::from_secs(5);
        while pid_alive(pid) && Instant::now() < gone {
            thread::sleep(Duration::from_millis(10));
        }
        pid
    }
}

/// Council item 4: no data is replaced while a server a dead supervisor left still runs, even
/// with no launch record left to say so.
#[test]
fn a_snapshot_is_never_restored_under_a_server_a_dead_supervisor_left_running() {
    let fixture = Fixture::with_bundle(WEB);
    let (_port, data) = up_with_data(&fixture, &[], 64);
    fs::write(data.join("rows"), "before").unwrap();
    fixture.ok(&["snapshot", "save", "base"]);
    fs::write(data.join("rows"), "after").unwrap();
    let server = fixture.session()["services"]["web"]["pid"].as_u64().unwrap() as u32;
    fixture.kill_supervisor();
    fixture.remove_session_records();
    let out = fixture.command(&["snapshot", "restore", "base", "--json"]).output().unwrap();
    assert!(!out.status.success(), "{}", json_result(&out));
    assert!(pid_alive(server));
    assert_eq!(fs::read_to_string(data.join("rows")).unwrap(), "after", "data replaced under a running server");
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(server));
}

/// Council item 8: each preset service runs its own pinned release, of any major.
#[test]
fn each_preset_service_runs_its_own_pinned_release() {
    for (a, b) in [("17.6", "18.1"), ("17.6", "17.11")] {
        let fixture = Fixture::with_bundle(&format!("[bundle]\nname='test'\n[services.pga]\npreset='postgres'\nversion='{a}'\n[services.pgb]\npreset='postgres'\nversion='{b}'\n"));
        let bin = fixture.dir.path().join("bin");
        servers::install(&bin);
        let (da, db) = (servers::postgres_release(fixture.dir.path(), &bin, a), servers::postgres_release(fixture.dir.path(), &bin, b));
        let path = format!("{}:{}:{}:{}", da.display(), db.display(), bin.display(), std::env::var("PATH").unwrap());
        fs::write(fixture.dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
        // (Both share DATABASE_URL, so only one of them verifies as its instance.)
        let _ = fixture.command(&["up", "--timeout", "10s"]).output().unwrap();
        for (service, version) in [("pga", a), ("pgb", b)] {
            let port = assigned_port(&fixture, service);
            let ran = fs::read_to_string(fixture.dir.path().join(format!("postgres-{port}.version"))).unwrap_or_default();
            assert_eq!(ran.trim(), version, "{service} ran another release");
        }
        fixture.ok(&["down"]);
    }
}

/// Council item 9: dropping the Pitchfork pin an older Stack left in stack.lock leaves the
/// running session current.
#[test]
fn compiling_away_the_old_pitchfork_pin_keeps_the_running_session_current() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let lock = fixture.dir.path().join("app/stack.lock");
    let text = fs::read_to_string(&lock).unwrap();
    fs::write(&lock, format!("{text}\n[[tool]]\nname = \"pitchfork\"\nrequested = \"2.29.0\"\nresolved = \"2.29.0\"\n")).unwrap();
    fixture.ok(&["compile", "--locked"]);
    let up = json_result(&fixture.ok(&["up", "--json"]));
    fixture.ok(&["compile"]);
    assert!(!fs::read_to_string(&lock).unwrap().contains("pitchfork"));
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!((status["data"]["stale"].clone(), status["data"]["healthy"].clone()), (json!(false), json!(true)), "{status}");
    assert_eq!(json_result(&fixture.ok(&["up", "--json"]))["data"]["session"]["id"], up["data"]["session"]["id"], "nothing restarted");
    fixture.ok(&["down"]);
}

/// Council item 10: two checkouts run a preset with further ports at once, each answering its
/// own readiness check.
#[test]
fn two_checkouts_run_a_preset_with_further_ports_at_once() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[services.queue]\npreset='nats'\nversion='2.10.1'\n");
    servers::install_nats(&fixture.dir.path().join("bin"));
    let other = fixture.dir.path().join("app2");
    fs::create_dir(&other).unwrap();
    fs::copy(fixture.dir.path().join("app/stack.toml"), other.join("stack.toml")).unwrap();
    fixture.ok(&["compile"]);
    assert!(fixture.command_at(&other, &["compile"]).output().unwrap().status.success());
    let monitor = |dir: &Path| -> String {
        let config = fs::read_to_string(dir.join(".config/mise/conf.d/stack.toml")).unwrap();
        config.lines().find(|l| l.starts_with("NATS_MONITOR_PORT")).unwrap().to_string()
    };
    let (mine, theirs) = (monitor(&fixture.dir.path().join("app")), monitor(&other));
    assert_ne!(mine, theirs);
    assert!(!mine.contains("8222"), "{mine}");
    fixture.ok(&["up"]);
    let out = fixture.command_at(&other, &["up", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", json_result(&out));
    fixture.ok(&["down"]);
    assert!(fixture.command_at(&other, &["down"]).output().unwrap().status.success());
}

/// Council item 12: at the deadline a healthy service is reported as it last verified, and the
/// one still starting as not verified in time.
#[test]
fn at_the_deadline_only_the_service_still_starting_is_reported() {
    let fixture = Fixture::with_bundle(&format!("[bundle]\nname='test'\n[services.web]\nrun='''{LISTEN}'''\n[services.slow]\nrun='exec sleep 600'\nready_port=1\n"));
    let out = fixture.command(&["up", "--timeout", "4s", "--json"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "timed_out", "{result}");
    let cause = &result["error"]["details"][0]["cause"];
    let failed: Vec<&str> = cause["details"].as_array().unwrap().iter().filter_map(|d| d["service"].as_str()).collect();
    assert_eq!(failed, ["slow"], "{result}");
    assert!(cause["details"][0]["reason"].as_str().unwrap().contains("not verified before the deadline"), "{result}");
    fixture.ok(&["down"]);
}

/// Council item 13: a Pitchfork supervisor the migrating `up` could not stop is reported, and
/// the next `down` tries again.
#[test]
fn a_pitchfork_supervisor_kept_during_migration_is_tried_again_by_down() {
    let fixture = Fixture::with_bundle(WEB);
    let pf = fixture.dir.path().join("pf");
    let legacy = fixture.legacy_record(&pf);
    let mark = "stack-2222222222222222bbbbbbbbbbbbbbbb".to_string();
    fs::write(fixture.dir.path().join("state/supervisor-owner"), format!("{mark}\n")).unwrap();
    let pid = fake_supervisor(&fixture, Some(&mark));
    fs::write(fixture.dir.path().join("pf-list.json"), json!([{ "id": "someone-else/db", "status": "running" }]).to_string()).unwrap();
    legacy();
    let up = json_result(&fixture.ok(&["up", "--json"]));
    let warning = up["data"]["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).find(|w| w.contains("Pitchfork supervisor")).unwrap_or_else(|| panic!("{up}")).to_string();
    assert!(warning.contains("someone-else/db is running") && warning.contains("`stack down` tries again"), "{warning}");
    assert!(pid_alive(pid), "a supervisor others use was stopped");
    fs::remove_file(fixture.dir.path().join("pf-list.json")).unwrap();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["supervisor"]["pitchfork"]["stopped"], true, "{down}");
    assert!(!pid_alive(pid));
}

/// Council item 16: `gc` shuts down a supervisor still running for a deleted checkout whose
/// session record is gone, and stops its services, before removing its directory.
#[test]
fn gc_stops_a_live_supervisor_a_deleted_checkout_left_without_a_session() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let service = service_pid(&fixture);
    let supervisor = fixture.supervisor_state()["supervisor"]["pid"].as_u64().unwrap() as u32;
    let dir = fixture.supervisor_dir();
    fixture.remove_session_records();
    fs::remove_dir_all(fixture.dir.path().join("app")).unwrap();
    let gc = json_result(&fixture.command_at(fixture.dir.path(), &["gc", "--json"]).output().unwrap());
    assert_eq!(gc["ok"], true, "{gc}");
    assert!(!pid_alive(service) && !pid_alive(supervisor));
    assert!(!dir.exists());
}

/// Council item 17: the supervisor's own log is cut to its tail before it grows past a bound.
#[test]
fn the_supervisors_own_log_is_bounded() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    let dir = fixture.supervisor_dir();
    fs::create_dir_all(&dir).unwrap();
    let line = "an old line of the supervisor's own\n";
    fs::write(dir.join("supervisor.log"), line.repeat(3 * 1024 * 1024 / line.len()) + "the last old line\n").unwrap();
    fixture.ok(&["up"]);
    let log = fs::read_to_string(dir.join("supervisor.log")).unwrap();
    assert!(log.len() <= 300 * 1024, "{} bytes", log.len());
    assert!(log.contains("the last old line") && log.starts_with(line), "the tail is kept, from a line");
    fixture.ok(&["down"]);
}

/// Restored from the Pitchfork era: an interrupted `up` leaves nothing `down` cannot stop, and
/// nothing starts after it.
#[test]
fn interrupting_up_leaves_nothing_down_cannot_stop() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::with_bundle(&format!(
        "[bundle]\nname='test'\n[services.db]\nrun='''{LISTEN}'''\n[services.web]\nrun='exec sleep 600'\ndepends_on=['db']\nsetup='touch \"$REVIEW_FIXTURE/in-setup\"; sleep 30'\n"
    ));
    let mut up = fixture.command(&["up", "--json"]).process_group(0).stdout(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.dir.path().join("in-setup").exists() {
        assert!(Instant::now() < deadline, "the setup never ran");
        thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::kill(-(up.id() as i32), libc::SIGINT) };
    assert!(!up.wait().unwrap().success());
    let db = service_pid(&fixture);
    assert!(fixture.supervisor_state()["services"].get("web").is_none(), "web started after the interrupt");
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!pid_alive(db));
    assert!(std::net::TcpStream::connect(("127.0.0.1", assigned_port(&fixture, "db"))).is_err());
}

/// Restored from the Pitchfork era: without a launch record, what the supervisor runs on the
/// checkout's port is still the checkout's to stop, and `down` waits for it.
#[test]
fn without_a_record_down_stops_what_the_supervisor_runs_and_waits_for_its_port() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    fixture.ok(&["up"]);
    let service = service_pid(&fixture);
    fixture.remove_session_records();
    let down = json_result(&fixture.ok(&["down", "--json"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(down["data"]["conflicts"].is_null(), "its own service is not foreign: {down}");
    assert!(!pid_alive(service));
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
}

/// Restored from the Pitchfork era: a service process of this checkout listening on its port
/// with nothing recording it (its record never written) is stopped by `up`, never called
/// foreign. See also `a_service_process_the_supervisor_lost_is_stopped_by_up_and_down_never_called_foreign`.
#[test]
fn a_launched_but_unrecorded_service_is_stopped_by_up_and_replaced() {
    let fixture = Fixture::with_bundle(WEB);
    let port = assigned_port(&fixture, "web");
    supervise_listener(&fixture, port);
    fixture.ok(&["up"]);
    let first = service_pid(&fixture);
    // Its supervisor dies and the record of the service is lost.
    fixture.kill_supervisor();
    fs::remove_file(fixture.supervisor_dir().join("state.json")).unwrap();
    fixture.remove_session_records();
    let up = json_result(&fixture.ok(&["up", "--json"]));
    assert!(!pid_alive(first), "the unrecorded process was left running: {up}");
    assert_eq!(up["data"]["checks"][0]["ready"], true, "{up}");
    fixture.ok(&["down"]);
}

// ---- unreadable session records ----------------------------------------------------------

fn index_record(fixture: &Fixture) -> std::path::PathBuf {
    fs::read_dir(fixture.dir.path().join("state/sessions")).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "json")).unwrap()
}

fn supervisor_record_dir(fixture: &Fixture) -> std::path::PathBuf {
    fs::read_dir(fixture.dir.path().join("state/supervisors")).unwrap().next().unwrap().unwrap().path()
}

fn running_pid(fixture: &Fixture) -> u32 {
    fs::read_to_string(fixture.dir.path().join("pf-tracked-pid")).unwrap().trim().parse().unwrap()
}

/// `args` fails `state_invalid`, naming `stack down`.
fn refused_unreadable(fixture: &Fixture, args: &[&str]) {
    let mut with_json = vec!["--json"];
    with_json.extend_from_slice(args);
    let out = fixture.command(&with_json).output().unwrap();
    let result = json_result(&out);
    assert!(!out.status.success(), "{args:?} succeeded: {result}");
    assert_eq!(result["error"]["code"], "state_invalid", "{args:?}: {result}");
    assert!(result["error"]["hint"].as_str().unwrap_or_default().contains("stack down"), "{args:?}: {result}");
}

#[test]
fn an_unreadable_session_record_stops_every_command_but_down_which_recovers_it() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let pid = running_pid(&fixture);
    let ports_before = fs::read_to_string(fixture.dir.path().join("state/ports.json")).unwrap();
    // A machine crash left the index record empty.
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    for args in [&["status"][..], &["exec", "--", "true"], &["env"], &["up"], &["restart"]] {
        refused_unreadable(&fixture, args);
    }
    // Nothing was started beside the running service, or stopped.
    assert!(pid_alive(pid));
    assert_eq!(running_pid(&fixture), pid, "another instance was started");
    // `gc` reports it, and stops, reclaims and releases nothing of it.
    let out = fixture.command(&["--json", "gc"]).output().unwrap();
    let gc = json_result(&out);
    assert_eq!(gc["error"]["code"], "gc_incomplete", "{gc}");
    assert!(gc["error"]["details"].to_string().contains("session record unreadable"), "{gc}");
    assert!(gc["error"]["hint"].as_str().unwrap().contains("stack down"), "{gc}");
    assert!(pid_alive(pid));
    assert_eq!(fs::read_to_string(&index).unwrap(), "", "gc touched the record");
    assert_eq!(fs::read_to_string(fixture.dir.path().join("state/ports.json")).unwrap(), ports_before);
    // `down` stops what the supervisor recorded and only then removes the record.
    let down = json_result(&fixture.ok(&["--json", "down"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(down["data"]["removed_unreadable"].to_string().contains(index.file_name().unwrap().to_str().unwrap()), "{down}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_alive(pid));
    assert!(!index.exists());
    let status = json_result(&fixture.command(&["status", "--json"]).output().unwrap());
    assert_eq!((status["ok"].clone(), status["data"]["session"].clone()), (json!(true), json!(null)), "{status}");
}

#[test]
fn an_unreadable_project_copy_is_not_overridden_by_a_readable_index_nor_reclaimed_by_gc() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up", "--ttl", "1s"]);
    let pid = running_pid(&fixture);
    let copy = fixture.dir.path().join("app/.stack/session.json");
    fs::write(&copy, "{\"id\":").unwrap();
    refused_unreadable(&fixture, &["status"]);
    // The lease expired: still nothing is reclaimed while the record cannot be read.
    thread::sleep(Duration::from_millis(2200));
    let gc = json_result(&fixture.command(&["--json", "gc"]).output().unwrap());
    assert!(gc["error"]["details"].to_string().contains("session record unreadable"), "{gc}");
    assert!(pid_alive(pid), "gc stopped a checkout whose record it could not read");
    assert!(index_record(&fixture).exists());
    // The pass inside another `up` leaves it alone too (and says so); this checkout's own `up`
    // refuses.
    refused_unreadable(&fixture, &["up"]);
    assert!(pid_alive(pid));
    fixture.ok(&["down"]);
    assert!(!copy.exists());
}

#[test]
fn down_without_the_supervisors_own_record_of_this_checkout_keeps_everything() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let pid = running_pid(&fixture);
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    let dir = supervisor_record_dir(&fixture);
    let spec = fs::read(dir.join("spec.json")).unwrap();
    // Unreadable, then naming another checkout: refused, nothing stopped or removed.
    fs::write(dir.join("spec.json"), "{").unwrap();
    let result = json_result(&fixture.command(&["--json", "down"]).output().unwrap());
    assert_eq!(result["error"]["code"], "state_invalid", "{result}");
    assert!(result["error"]["message"].as_str().unwrap().contains("nothing was stopped and every record was kept"), "{result}");
    let mut other: Value = serde_json::from_slice(&spec).unwrap();
    other["project"] = json!("/elsewhere");
    fs::write(dir.join("spec.json"), other.to_string()).unwrap();
    let out = fixture.command(&["--json", "down"]).output().unwrap();
    let result = json_result(&out);
    assert_eq!(result["error"]["code"], "state_invalid", "{result}");
    assert!(result["error"]["message"].as_str().unwrap().contains("not this checkout's"), "{result}");
    assert!(pid_alive(pid));
    assert!(index.exists() && fs::read(&index).unwrap().is_empty());
    // With its own record back, `down` recovers.
    fs::write(dir.join("spec.json"), &spec).unwrap();
    fixture.ok(&["down"]);
    assert!(!index.exists());
}

#[test]
fn down_recovering_an_unreadable_record_never_signals_a_process_the_supervisor_record_does_not_match() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let service = running_pid(&fixture);
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    // The supervisor dies; its record now names, by pid only, an unrelated process (the
    // recorded start time is the service's, not this process's).
    let dir = supervisor_record_dir(&fixture);
    let mut state: Value = serde_json::from_slice(&fs::read(dir.join("state.json")).unwrap()).unwrap();
    let supervisor = state["supervisor"]["pid"].as_u64().unwrap() as i32;
    unsafe { libc::kill(supervisor, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(supervisor as u32) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let mut bystander = Command::new("sleep").arg("30").spawn().unwrap();
    state["services"]["web"]["pid"] = json!(bystander.id());
    fs::write(dir.join("state.json"), state.to_string()).unwrap();
    let out = fixture.command(&["--json", "down"]).output().unwrap();
    let bystander_alive = bystander.try_wait().unwrap().is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(bystander_alive, "down signalled a process its record did not match: {}", String::from_utf8_lossy(&out.stdout));
    // The service itself (carrying the checkout's token) was stopped, and only then the record
    // removed.
    assert_eq!(json_result(&out)["data"]["confirmed"], true, "{}", String::from_utf8_lossy(&out.stdout));
    assert!(!pid_alive(service));
    assert!(!index.exists());
}

/// Up a listening `web`, then empty its index record: the checkout and its record.
fn unreadable_with_service() -> (Fixture, std::path::PathBuf) {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up"]);
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    (fixture, index)
}

fn edit_state(fixture: &Fixture, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let path = supervisor_record_dir(fixture).join("state.json");
    let before = fs::read(&path).unwrap();
    let mut state: Value = serde_json::from_slice(&before).unwrap();
    edit(&mut state);
    fs::write(&path, state.to_string()).unwrap();
    before
}

fn down_refused(fixture: &Fixture, says: &str) {
    let result = json_result(&fixture.command(&["--json", "down"]).output().unwrap());
    assert_eq!(result["error"]["code"], "state_invalid", "{result}");
    let message = result["error"]["message"].as_str().unwrap();
    assert!(message.contains(says) && message.contains("every record was kept"), "{result}");
}

#[test]
fn recovery_needs_a_supervisor_state_that_accounts_for_every_service() {
    let (fixture, index) = unreadable_with_service();
    let pid = running_pid(&fixture);
    let state = supervisor_record_dir(&fixture).join("state.json");
    // `{}` parses, and is no evidence of anything.
    let original = fs::read(&state).unwrap();
    fs::write(&state, "{}").unwrap();
    down_refused(&fixture, "records no request");
    // A state that does not record a service the spec names.
    fs::write(&state, &original).unwrap();
    edit_state(&fixture, |s| {
        s["services"].as_object_mut().unwrap().remove("web");
    });
    down_refused(&fixture, "records nothing of web");
    // A process recorded without its boot.
    fs::write(&state, &original).unwrap();
    edit_state(&fixture, |s| {
        s["services"]["web"]["process"].as_object_mut().unwrap().remove("boot");
    });
    down_refused(&fixture, "without the boot");
    assert!(pid_alive(pid), "a refused recovery stopped something");
    assert!(fs::read(&index).unwrap().is_empty());
    // With the evidence back, it recovers.
    fs::write(&state, &original).unwrap();
    fixture.ok(&["down"]);
    assert!(!index.exists() && !pid_alive(pid));
}

#[test]
fn recovery_after_a_reboot_takes_an_earlier_boots_record_as_proof_its_processes_are_gone() {
    let (fixture, index) = unreadable_with_service();
    // The machine went down: nothing of the checkout runs, and the supervisor's record is of
    // the boot before.
    let service = running_pid(&fixture);
    let supervisor = supervisor_pid(&fixture);
    for pid in [supervisor, service] {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while (pid_alive(service) || pid_alive(supervisor)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    edit_state(&fixture, |s| {
        s["supervisor"]["boot"] = json!("an-earlier-boot");
        s["services"]["web"]["process"]["boot"] = json!("an-earlier-boot");
    });
    let down = json_result(&fixture.ok(&["--json", "down"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert!(!index.exists());
}

fn supervisor_pid(fixture: &Fixture) -> u32 {
    let state: Value = serde_json::from_slice(&fs::read(supervisor_record_dir(fixture).join("state.json")).unwrap()).unwrap();
    state["supervisor"]["pid"].as_u64().unwrap() as u32
}

#[test]
fn an_unreadable_project_copy_is_reported_by_gc_and_its_dry_run_alike_before_and_after_expiry() {
    let fixture = Fixture::with_bundle(WEB);
    supervise_listener(&fixture, assigned_port(&fixture, "web"));
    fixture.ok(&["up", "--ttl", "2s"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    fs::write(app.join(".stack/session.json"), "").unwrap();
    for _ in ["before expiry", "after expiry"] {
        let (ok, gc) = fixture.gc(&[]);
        assert!(!ok && gc["error"]["details"][0]["reason"] == "session record unreadable", "{gc}");
        assert_eq!(gc["error"]["details"][0]["project"], json!(app), "{gc}");
        let (ok, plan) = fixture.gc(&["--dry-run"]);
        assert!(ok, "{plan}");
        assert_eq!(plan["data"]["would_reclaim"], json!([]), "{plan}");
        assert_eq!(plan["data"]["unreadable"][0]["project"], json!(app), "{plan}");
        thread::sleep(Duration::from_millis(2500));
    }
    assert!(pid_alive(running_pid(&fixture)));
    fixture.ok(&["down"]);
}

#[test]
fn an_unreadable_record_names_its_checkout_where_another_record_does() {
    let (fixture, index) = unreadable_with_service();
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    // gc and ps name the checkout (from the supervisor's spec and the port registry), not the
    // record file; ps lists it as unreadable instead of failing.
    let (_, gc) = fixture.gc(&[]);
    assert_eq!(gc["error"]["details"][0]["project"], json!(app), "{gc}");
    assert!(gc["error"]["details"][0]["error"].as_str().unwrap().contains(&format!("checkout {}", app.display())), "{gc}");
    let ps = json_result(&fixture.ok(&["--json", "ps", "--no-size"]));
    let row = ps["data"]["checkouts"].as_array().unwrap().iter().find(|c| c["path"] == json!(app)).cloned().unwrap_or_else(|| panic!("{ps}"));
    assert_eq!(row["state"], "unreadable", "{ps}");
    assert!(row["unreadable"].as_str().unwrap().contains(index.file_name().unwrap().to_str().unwrap()), "{ps}");
    fixture.ok(&["down"]);
}

#[test]
fn a_tools_only_checkouts_unreadable_record_is_cleared_by_down_and_listed_by_ps() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    fixture.ok(&["up"]);
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    let ps = json_result(&fixture.ok(&["--json", "ps", "--no-size"]));
    assert!(ps["data"]["checkouts"].as_array().unwrap().iter().any(|c| c["state"] == "unreadable"), "{ps}");
    // No supervisor ever ran for it and no port is reserved for it: nothing of it can run.
    assert!(!fixture.dir.path().join("state/supervisors").exists());
    let down = json_result(&fixture.ok(&["--json", "down"]));
    assert_eq!(down["data"]["confirmed"], true, "{down}");
    assert_eq!(down["data"]["recovery_checked"], stack::session::TOOLS_ONLY_CHECKED, "{down}");
    assert!(!index.exists());
    fixture.ok(&["status", "--json"]);
}

/// macOS hides the environment of its own binaries: a process of them carrying the checkout's
/// token is not seen. `down` clears the record, says what it could and could not check, and
/// signals nothing.
#[cfg(target_os = "macos")]
#[test]
fn clearing_a_tools_only_record_says_what_was_checked_and_never_signals_an_unseen_token_carrier() {
    let fixture = Fixture::with_bundle("[bundle]\nname='test'\n[tools]\njq='1.7.1'\n");
    fixture.ok(&["up"]);
    let app = fixture.dir.path().join("app").canonicalize().unwrap();
    let token = stack::checkout::token(&app).unwrap();
    let mut carrier = Command::new("/bin/sleep").arg("30").env(stack::checkout::TOKEN_VAR, &token).spawn().unwrap();
    thread::sleep(Duration::from_millis(100));
    if stack::procinfo::carrying(stack::checkout::TOKEN_VAR, &token).iter().any(|p| p.pid == carrier.id()) {
        let _ = carrier.kill();
        let _ = carrier.wait();
        eprintln!("skipping: this machine shows /bin/sleep's environment");
        return;
    }
    let index = index_record(&fixture);
    fs::write(&index, "").unwrap();
    let out = fixture.ok(&["down"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let alive = carrier.try_wait().unwrap().is_none();
    let _ = carrier.kill();
    let _ = carrier.wait();
    assert!(alive, "down signalled a process it could not see");
    assert!(text.contains(&format!("checked: {}", stack::session::TOOLS_ONLY_CHECKED)), "{text}");
    assert!(text.contains("a process whose environment cannot be read is not seen") && text.contains("the command registry, and their mark"), "{text}");
    assert!(!index.exists());
}
