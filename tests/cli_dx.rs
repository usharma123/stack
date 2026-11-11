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
fn mcp_dir_means_exactly_that_directory_and_never_the_servers_own() {
    let fixture = Fixture::new("[bundle]\nname='test'\n");
    let app = fixture.app();
    let deep = app.join("src/deep");
    // Started inside the checkout: the server's working directory still decides nothing.
    let replies = fixture.mcp(&deep, &[("stack_inspect", json!({ "dir": app })), ("stack_inspect", json!({ "dir": deep })), ("stack_inspect", json!({}))]);
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    assert_eq!(replies[1]["error"]["code"], "manifest_missing", "{}", replies[1]);
    assert_eq!(replies[1]["project"], deep.to_str().unwrap());
    assert_eq!(replies[2]["error"]["code"], "usage", "{}", replies[2]);
}
