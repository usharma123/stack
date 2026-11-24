#![cfg(unix)]
//! Labels, `ps` selections and the fleet forms of `status`, `logs` and `down`, across two
//! worktrees of one repository running a listening service under Stack's own supervisor (with
//! the fake mise in tests/fakes).

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

/// A service that listens on its port, says so, and waits.
const LISTEN: &str = r#"exec "$REVIEW_PYTHON" -c 'import os,socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(os.environ["PORT"]))); s.listen(); print("web listening", flush=True); time.sleep(600)'"#;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// `repo` (branch main) and its worktree `wt` (branch feature), each a checkout of the
    /// bundle's one service, compiled; `extra`, a third worktree with a stack.toml that nothing
    /// was ever run in.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bin", "bundle", "repo"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("bundle/bundle.toml"), format!("[bundle]\nname='test'\n[services.web]\nrun='''{LISTEN}'''\n")).unwrap();
        fs::write(dir.path().join("daemons.json"), "[]").unwrap();
        let path = format!("{}:{}", dir.path().join("bin").display(), std::env::var("PATH").unwrap());
        fs::write(dir.path().join("env.json"), json!({ "PATH": path }).to_string()).unwrap();
        let mise = dir.path().join("bin/mise");
        fs::write(&mise, FAKE_MISE).unwrap();
        fs::set_permissions(mise, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self { dir };
        let repo = fixture.path("repo");
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n").unwrap();
        fs::write(repo.join(".gitignore"), ".stack/\nstack.lock\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "stack"]);
        git(&repo, &["worktree", "add", "-q", "-b", "feature", fixture.dir.path().join("wt").to_str().unwrap()]);
        git(&repo, &["worktree", "add", "-q", "-b", "extra", fixture.dir.path().join("extra").to_str().unwrap()]);
        for checkout in ["repo", "wt"] {
            fixture.ok(&["-C", fixture.path(checkout).to_str().unwrap(), "compile"]);
        }
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name).canonicalize().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .current_dir(self.path("repo"))
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

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    /// `stack <args> --json`'s `data`, which must have succeeded.
    fn data(&self, args: &[&str]) -> Value {
        let out = self.run(&[args, &["--json"]].concat());
        let result = json_result(&out);
        assert!(out.status.success() && result["ok"] == true, "{args:?}: {result}");
        result["data"].clone()
    }

    /// Answers to `calls` made over one MCP session, in call order.
    fn mcp(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut input = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string() + "\n";
        for (i, (name, args)) in calls.iter().enumerate() {
            input += &(json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string() + "\n");
        }
        let mut child = self.command(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let mut answered: Vec<Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|m| m["id"].as_u64().is_some_and(|id| id > 0))
            .collect();
        answered.sort_by_key(|m| m["id"].as_u64());
        answered.iter().map(|m| m["result"]["structuredContent"].clone()).collect()
    }
}

impl Drop for Fixture {
    /// Nothing a test started outlives it, whatever it asserted.
    fn drop(&mut self) {
        for checkout in ["repo", "wt"] {
            let _ = self.command(&["-C", self.dir.path().join(checkout).to_str().unwrap(), "down"]).output();
        }
    }
}

fn git(at: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn json_result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The checkouts of a fleet report or `ps`, by path.
fn paths(report: &Value) -> Vec<PathBuf> {
    report["checkouts"].as_array().unwrap().iter().map(|c| PathBuf::from(c["path"].as_str().unwrap())).collect()
}

fn row<'a>(report: &'a Value, path: &Path) -> &'a Value {
    report["checkouts"].as_array().unwrap().iter().find(|c| Path::new(c["path"].as_str().unwrap()) == path).unwrap_or_else(|| panic!("{path:?} in {report}"))
}

#[test]
fn labels_are_checked_recorded_merged_and_kept_by_up() {
    let fixture = Fixture::new();
    let repo = fixture.path("repo");
    let repo_dir = repo.to_str().unwrap();

    // A malformed label is a usage error, with a hint, before anything starts.
    for bad in ["nope", "bad key=v", "=v"] {
        let out = fixture.run(&["-C", repo_dir, "up", "--label", bad, "--json"]);
        let result = json_result(&out);
        assert_eq!(out.status.code(), Some(2), "{bad}: {result}");
        assert_eq!(result["error"]["code"], "usage", "{result}");
        assert!(result["error"]["hint"].as_str().unwrap().contains("KEY=VALUE"), "{result}");
    }
    assert!(!repo.join(".stack/session.json").exists(), "nothing was started");

    let up = fixture.data(&["-C", repo_dir, "up", "--label", "agent=claude", "--label", "task=a"]);
    assert_eq!(up["session"]["labels"], json!({ "agent": "claude", "task": "a" }), "{up}");
    let id = up["session"]["id"].clone();
    // Kept running: a new label is added, a given key replaced, the session the same.
    let up = fixture.data(&["-C", repo_dir, "up", "--label", "task=b", "--label", "owner=team/x"]);
    assert_eq!(up["session"]["labels"], json!({ "agent": "claude", "owner": "team/x", "task": "b" }), "{up}");
    assert_eq!(up["session"]["id"], id, "the running session is kept");
    // No label given: the labels stay.
    let up = fixture.data(&["-C", repo_dir, "up"]);
    assert_eq!(up["session"]["labels"], json!({ "agent": "claude", "owner": "team/x", "task": "b" }), "{up}");
    let status = fixture.data(&["-C", repo_dir, "status"]);
    assert_eq!(status["session"]["labels"]["agent"], "claude", "{status}");

    // MCP: labels as an object, merged the same way.
    let answers = fixture.mcp(&[("stack_up", json!({ "dir": repo, "labels": { "task": "c" } }))]);
    assert_eq!(answers[0]["data"]["session"]["labels"], json!({ "agent": "claude", "owner": "team/x", "task": "c" }), "{}", answers[0]);
    let answers = fixture.mcp(&[("stack_up", json!({ "dir": repo, "labels": { "task": 5 } }))]);
    assert_eq!(answers[0]["error"]["code"], "usage", "{}", answers[0]);

    // `down` ends the session, and its labels with it.
    fixture.ok(&["-C", repo_dir, "down"]);
    let up = fixture.data(&["-C", repo_dir, "up"]);
    assert!(up["session"].get("labels").is_none(), "{up}");
}

#[test]
fn ps_status_logs_and_down_act_on_a_projects_worktrees_or_a_label() {
    let fixture = Fixture::new();
    let (repo, wt, extra) = (fixture.path("repo"), fixture.path("wt"), fixture.path("extra"));
    let (repo_dir, wt_dir) = (repo.to_str().unwrap(), wt.to_str().unwrap());
    fixture.ok(&["-C", repo_dir, "up", "--label", "agent=claude", "--label", "task=api"]);
    fixture.ok(&["-C", wt_dir, "up", "--label", "agent=codex"]);

    // ---- ps ----
    let ps = fixture.data(&["ps", "--no-size"]);
    let r = row(&ps, &repo);
    assert_eq!((r["state"].as_str(), r["branch"].as_str()), (Some("running"), Some("main")), "{r}");
    assert_eq!(r["labels"], json!({ "agent": "claude", "task": "api" }), "{r}");
    assert_eq!(r["head"].as_str().map(str::len), Some(7), "{r}");
    let port = r["services"][0]["port"].as_u64().unwrap();
    assert_eq!(r["services"][0]["endpoint"], format!("http://127.0.0.1:{port}"), "a custom service's endpoint: {r}");
    let w = row(&ps, &wt);
    assert_eq!((w["branch"].as_str(), w["project_id"].clone()), (Some("feature"), r["project_id"].clone()), "one project: {w}");
    assert!(r["project_id"].is_string(), "{r}");

    // From the repository's main checkout, --project is its own project: both worktrees.
    assert_eq!(paths(&fixture.data(&["ps", "--no-size", "--project"])), vec![repo.clone(), wt.clone()]);
    assert_eq!(paths(&fixture.data(&["ps", "--no-size", &format!("--project={wt_dir}")])), vec![repo.clone(), wt.clone()]);
    assert_eq!(paths(&fixture.data(&["ps", "--no-size", "--label", "agent=codex"])), vec![wt.clone()]);
    assert_eq!(paths(&fixture.data(&["ps", "--no-size", "--label", "agent=claude", "--label", "task=api"])), vec![repo.clone()]);
    assert!(paths(&fixture.data(&["ps", "--no-size", "--state", "stopped", "--project"])).is_empty());
    let out = fixture.run(&["ps", "--state", "nope", "--json"]);
    assert_eq!((out.status.code(), json_result(&out)["error"]["code"].clone()), (Some(2), json!("usage")));
    // --worktrees adds the worktree with a stack.toml and nothing recorded.
    let ps = fixture.data(&["ps", "--no-size", "--project", "--worktrees"]);
    assert_eq!(paths(&ps), vec![extra.clone(), repo.clone(), wt.clone()]);
    let e = row(&ps, &extra);
    assert_eq!((e["state"].as_str(), e["dir"].as_str(), e["dir_exists"].as_bool(), e["branch"].as_str()), (Some("unstarted"), Some("present"), Some(true), Some("extra")), "{e}");
    assert_eq!(paths(&fixture.data(&["ps", "--no-size", "--worktrees", "--state", "unstarted"])), vec![extra.clone()]);
    let human = stdout(&fixture.ok(&["ps", "--no-size", "--project"]));
    assert!(human.contains("branch main @ ") && human.contains("labels agent=claude,task=api") && human.contains(&format!("http://127.0.0.1:{port}")), "{human}");
    // MCP: the same selections.
    let answers = fixture.mcp(&[
        ("stack_ps", json!({ "dir": repo, "project": true, "no_size": true })),
        ("stack_ps", json!({ "labels": { "agent": "codex" }, "no_size": true })),
        ("stack_ps", json!({ "dir": repo, "worktrees": true, "state": "unstarted", "no_size": true })),
    ]);
    assert_eq!(paths(&answers[0]["data"]), vec![repo.clone(), wt.clone()], "{}", answers[0]);
    assert_eq!(paths(&answers[1]["data"]), vec![wt.clone()], "{}", answers[1]);
    assert_eq!(paths(&answers[2]["data"]), vec![extra.clone()], "{}", answers[2]);

    // ---- status ----
    let out = fixture.run(&["status", "--project", "--json"]);
    let result = json_result(&out);
    assert_eq!(out.status.code(), Some(0), "{result}");
    assert_eq!((result["ok"].clone(), result["passed"].clone()), (json!(true), json!(true)), "{result}");
    let status = &result["data"];
    assert_eq!(paths(status), vec![repo.clone(), wt.clone()], "{status}");
    assert_eq!((status["total"].as_u64(), status["running"].as_u64(), status["healthy"].as_u64()), (Some(2), Some(2), Some(2)), "{status}");
    assert_eq!(row(status, &wt)["branch"], "feature");
    assert_eq!(row(status, &wt)["status"]["healthy"], true, "{status}");
    assert_eq!(paths(&fixture.data(&["status", "--label", "agent=codex"])), vec![wt.clone()]);
    assert_eq!(paths(&fixture.data(&["status", "--all"])), vec![repo.clone(), wt.clone()], "extra has nothing recorded");
    let human = stdout(&fixture.ok(&["status", "--project"]));
    assert!(human.contains("[main]  healthy") && human.contains("[feature]  healthy") && human.contains("2 checkout(s): 2 started, 2 healthy"), "{human}");
    let out = fixture.run(&["status", "--all", "--project"]);
    assert_eq!(out.status.code(), Some(2), "--all and --project conflict");
    // Without a selection, status is the one checkout's, as before.
    let one = fixture.data(&["-C", wt_dir, "status"]);
    assert!(one["healthy"] == true && one.get("checkouts").is_none(), "{one}");

    // ---- logs ----
    let logs = fixture.data(&["logs", "--project", "--since-start"]);
    assert_eq!(paths(&logs), vec![repo.clone(), wt.clone()]);
    for c in logs["checkouts"].as_array().unwrap() {
        let lines = c["logs"]["services"]["web"]["lines"].as_array().unwrap_or_else(|| panic!("{c}"));
        assert!(lines.iter().any(|l| l == "web listening"), "{c}");
    }
    let human = stdout(&fixture.ok(&["logs", "--label", "agent=codex", "web"]));
    assert!(human.lines().any(|l| l == "[feature:web] web listening"), "{human}");
    assert!(!human.contains("[main:"), "{human}");
    let out = fixture.run(&["logs", "--project", "--follow"]);
    assert_eq!(out.status.code(), Some(2), "--follow cannot follow several checkouts");
    // A path after a space is not taken for a service: the call is refused with the spelling
    // that works, and `--project=PATH` selects that project.
    let out = fixture.run(&["--json", "logs", "--project", wt_dir, "--since-start"]);
    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    let refused: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(refused["error"]["code"], "usage", "{refused}");
    assert!(refused["error"]["hint"].as_str().unwrap().contains(&format!("stack logs --project={wt_dir}")), "{refused}");
    let logs = fixture.data(&["logs", &format!("--project={wt_dir}"), "--since-start"]);
    assert_eq!(paths(&logs), vec![repo.clone(), wt.clone()]);

    // ---- MCP status and logs by label, without a dir ----
    let answers = fixture.mcp(&[
        ("stack_status", json!({ "labels": { "agent": "claude" } })),
        ("stack_logs", json!({ "labels": { "agent": "claude" }, "service": "web" })),
        ("stack_status", json!({ "dir": wt, "project": true })),
    ]);
    assert_eq!((answers[0]["passed"].clone(), paths(&answers[0]["data"])), (json!(true), vec![repo.clone()]), "{}", answers[0]);
    assert!(answers[0].get("project").is_none(), "acted on no single checkout: {}", answers[0]);
    let lines = &answers[1]["data"]["checkouts"][0]["logs"]["lines"];
    assert!(lines.as_array().unwrap().iter().any(|l| l == "web listening"), "{}", answers[1]);
    assert_eq!(paths(&answers[2]["data"]), vec![repo.clone(), wt.clone()], "{}", answers[2]);
    // A purge of checkouts selected by label anywhere on the machine needs the server's consent:
    // refused, and nothing stopped.
    let answers = fixture.mcp(&[("stack_down", json!({ "labels": { "agent": "codex" }, "purge": true }))]);
    assert_eq!(answers[0]["error"]["code"], "host_purge_disabled", "{}", answers[0]);
    assert_eq!(row(&fixture.data(&["ps", "--no-size"]), &wt)["state"], "running");

    // ---- one checkout's service dies: the project's status fails ----
    let pid = row(&fixture.data(&["ps", "--no-size"]), &wt)["services"][0]["pid"].as_u64().unwrap();
    assert!(Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while row(&fixture.data(&["ps", "--no-size"]), &wt)["state"] == "running" {
        assert!(std::time::Instant::now() < deadline, "the killed service is still listed as running");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = fixture.run(&["status", "--project", "--json"]);
    let result = json_result(&out);
    assert_eq!(out.status.code(), Some(1), "{result}");
    assert_eq!((result["ok"].clone(), result["passed"].clone()), (json!(true), json!(false)), "{result}");
    let status = &result["data"];
    assert_eq!((status["running"].as_u64(), status["healthy"].as_u64(), status["passed"].as_bool()), (Some(2), Some(1), Some(false)), "{status}");
    assert_eq!(row(status, &wt)["status"]["healthy"], false, "{status}");
    let out = fixture.run(&["status", "--project"]);
    assert_eq!(out.status.code(), Some(1));
    let human = stdout(&out);
    assert!(human.contains("[feature]  NOT HEALTHY") && human.contains("web          NOT READY") && human.contains("[main]  healthy"), "{human}");
    // The healthy checkout alone, by its label, still passes.
    assert_eq!(fixture.run(&["status", "--label", "agent=claude"]).status.code(), Some(0));

    // ---- down ----
    let down = fixture.data(&["down", "--label", "agent=codex"]);
    assert_eq!((down["confirmed"].as_u64(), down["failed"].as_u64()), (Some(1), Some(0)), "{down}");
    assert_eq!(paths(&down), vec![wt.clone()]);
    assert_eq!(down["checkouts"][0]["down"]["confirmed"], true, "{down}");
    let ps = fixture.data(&["ps", "--no-size", "--project"]);
    assert_eq!(row(&ps, &repo)["state"], "running", "the other checkout keeps running: {ps}");
    assert!(row(&ps, &wt).get("labels").is_none(), "its session, and labels, are gone: {ps}");
    // Status of the project now: one started and healthy, one stopped; it still passes.
    let status = fixture.data(&["status", "--project"]);
    assert_eq!((status["running"].as_u64(), status["healthy"].as_u64(), status["passed"].as_bool()), (Some(1), Some(1), Some(true)), "{status}");
    // MCP: the project's down from one of its worktrees.
    let answers = fixture.mcp(&[("stack_down", json!({ "dir": wt, "project": true }))]);
    assert_eq!((answers[0]["passed"].clone(), answers[0]["data"]["confirmed"].clone()), (json!(true), json!(2)), "{}", answers[0]);
    assert!(paths(&fixture.data(&["ps", "--no-size", "--state", "running"])).is_empty());
    let human = stdout(&fixture.ok(&["down", "--project"]));
    assert!(human.contains("[main]  stopped 0 service(s); confirmed") && human.contains("2 checkout(s) stopped and confirmed, 0 failed"), "{human}");
    let out = fixture.run(&["down", "--label", "agent=codex", "--forget-unrecorded"]);
    assert_eq!(out.status.code(), Some(2), "--forget-unrecorded is for one checkout");
}
