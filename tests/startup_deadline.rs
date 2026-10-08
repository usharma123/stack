//! `up` and `restart` timeouts, through the CLI and MCP: a timeout too long to track is refused
//! before any work.
#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// A project in a directory of its own, run with every state, cache and config location inside
/// it and no provider on PATH.
struct Project {
    dir: TempDir,
}

impl Project {
    fn new(manifest: &str, lock: Option<&str>) -> Self {
        let dir = tempfile::Builder::new().prefix("stack-deadline-").tempdir().unwrap();
        let app = dir.path().join("app");
        fs::create_dir_all(&app).unwrap();
        for sub in ["bin", "home", "tmp"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        fs::write(app.join("stack.toml"), manifest).unwrap();
        if let Some(lock) = lock {
            fs::write(app.join("stack.lock"), lock).unwrap();
        }
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command.arg("-C").arg(self.path("app")).args(args).env_clear();
        command.env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()));
        for (var, rel) in [
            ("HOME", "home"),
            ("TMPDIR", "tmp"),
            ("XDG_CONFIG_HOME", "xdg/config"),
            ("XDG_DATA_HOME", "xdg/data"),
            ("XDG_CACHE_HOME", "xdg/cache"),
            ("XDG_STATE_HOME", "xdg/state"),
            ("STACK_STATE_DIR", "state"),
            ("STACK_DATA_DIR", "data"),
            ("STACK_CACHE_DIR", "cache"),
            ("MISE_DATA_DIR", "mise/data"),
            ("MISE_CONFIG_DIR", "mise/config"),
            ("MISE_CACHE_DIR", "mise/cache"),
            ("MISE_STATE_DIR", "mise/state"),
            ("MISE_GLOBAL_CONFIG_FILE", "mise/global.toml"),
            ("MISE_SYSTEM_CONFIG_FILE", "mise/system.toml"),
            ("PITCHFORK_CONFIG_DIR", "pitchfork/config"),
            ("PITCHFORK_STATE_DIR", "pitchfork/state"),
            ("PITCHFORK_LOGS_DIR", "pitchfork/logs"),
            ("UV_CACHE_DIR", "uv"),
        ] {
            command.env(var, self.path(rel));
        }
        command
    }

    /// Run `calls` through one MCP server; returns each call's result and how long it all took.
    fn mcp(&self, mut command: Command, calls: &[(&str, Value)]) -> (Vec<Value>, Duration) {
        let mut input = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string() + "\n";
        for (i, (name, args)) in calls.iter().enumerate() {
            let mut args = args.clone();
            args["dir"] = json!(self.path("app"));
            input += &format!("{}\n", json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call","params":{"name":name,"arguments":args}}));
        }
        let start = Instant::now();
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let took = start.elapsed();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let responses: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let results = (1..=calls.len()).map(|id| responses.iter().find(|r| r["id"] == id).unwrap()["result"].clone()).collect();
        (results, took)
    }

    /// Every file anything wrote outside the project's own sources.
    fn written(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.dir.path(), &mut out);
        out.retain(|p| !["app/stack.toml", "app/stack.lock"].iter().any(|own| p.ends_with(own)));
        out
    }
}

fn json_result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn a_timeout_too_long_to_track_is_refused_before_any_work() {
    let project = Project::new("", None);
    let max = u64::MAX.to_string();
    for args in [&["up", "--timeout", &format!("{max}s")][..], &["restart", "--timeout", &max]] {
        let out = project.command(&[&["--json"][..], args].concat()).output().unwrap();
        assert!(!out.status.success() && out.status.code() != Some(124), "{args:?}");
        let error = &json_result(&out)["error"];
        assert_eq!(error["code"], "usage", "{args:?}: {error}");
        assert_eq!(error["message"], format!("a startup timeout of {max}s is too long"), "{error}");
    }
    let (results, _) = project.mcp(
        project.command(&["mcp"]),
        &[
            ("stack_up", json!({ "timeout_secs": u64::MAX })),
            ("stack_restart", json!({ "timeout_secs": u64::MAX })),
        ],
    );
    for result in &results {
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(result["structuredContent"]["error"]["code"], "usage", "{result}");
    }
    // No collection, lock, session or provider call happened.
    assert_eq!(project.written(), Vec::<PathBuf>::new());
}
