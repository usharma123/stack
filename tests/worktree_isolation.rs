#![cfg(unix)]
//! The README's claim, as one test: several worktrees of one repository, each with its own real
//! Postgres and Redis (the presets, through the real mise on PATH and the releases already in its
//! installs; skipped when there is no mise or no such release), started at once, never see each
//! other's data, ports or services, and stop completely at once.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const WORKTREES: usize = 5;

const MANIFEST: &str = "[services.postgres]\npreset = 'postgres'\nversion = '17'\n\n[services.redis]\npreset = 'redis'\nversion = '8'\n";

/// mise's installs directory, when mise is on PATH and it holds a Postgres 17 and a Redis 8.
fn installs() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("mise")).find(|m| m.is_file())?;
    let data = std::env::var_os("MISE_DATA_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/mise")))?;
    let installs = data.join("installs");
    let has = |tool: &str, major: &str, bin: &str| {
        std::fs::read_dir(installs.join(tool)).into_iter().flatten().flatten().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name == major || name.starts_with(&format!("{major}."))) && e.path().join("bin").join(bin).is_file()
        })
    };
    (has("postgres", "17", "postgres") && has("postgres", "17", "psql") && has("redis", "8", "redis-server") && has("redis", "8", "redis-cli")).then_some(installs)
}

struct Fleet {
    dir: tempfile::TempDir,
    worktrees: Vec<PathBuf>,
}

fn stack_bin() -> PathBuf {
    std::env::var_os("STACK_TEST_BINARY").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_stack")))
}

fn require_installs() -> Option<PathBuf> {
    let found = installs();
    if found.is_none() && std::env::var_os("STACK_REQUIRE_WORKTREE_ISOLATION").is_some() {
        panic!("STACK_REQUIRE_WORKTREE_ISOLATION is set, but mise with Postgres 17 and Redis 8 was not found");
    }
    found
}

impl Fleet {
    fn stack(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(stack_bin())
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("STACK_STATE_DIR", self.dir.path().join("state"))
            .env("STACK_CACHE_DIR", self.dir.path().join("cache"))
            .env_remove("STACK_COMMAND_MARK")
            .env_remove("STACK_SESSION")
            .env_remove("DATABASE_URL")
            .env_remove("REDIS_URL")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// `--json <args>` in `dir`, which must succeed; its `data`.
    fn ok(&self, dir: &Path, args: &[&str]) -> Value {
        let out = self.stack(dir, &[&["--json"], args].concat());
        let result: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        assert!(out.status.success() && result["ok"] == true, "{} {args:?}: {result}", dir.display());
        result["data"].clone()
    }

    /// A shell command run through `stack exec` in `dir`, with only the environment Stack gives
    /// it; its standard output, trimmed.
    fn exec(&self, dir: &Path, script: &str) -> String {
        let data = self.ok(dir, &["exec", "--require-all", "--", "sh", "-c", script]);
        assert_eq!(data["exit_code"], 0, "{script}: {data}");
        data["stdout"].as_str().unwrap().trim().to_string()
    }

    /// `args` in every worktree at once.
    fn each(&self, args: &[&str]) -> Vec<Value> {
        std::thread::scope(|s| {
            let handles: Vec<_> = self.worktrees.iter().map(|w| s.spawn(move || self.ok(w, args))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }
}

impl Drop for Fleet {
    /// Nothing is left running, whatever failed.
    fn drop(&mut self) {
        for w in &self.worktrees {
            let _ = self.stack(w, &["down"]);
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn accepting(port: u64) -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port as u16)), Duration::from_millis(200)).is_ok()
}

/// Each worktree's marker: written through `stack exec` with only `DATABASE_URL` and
/// `REDIS_URL`, read back the same way.
const WRITE: &str = r#"psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -c "create table marker (name text); insert into marker values ('$MARK')" && redis-cli -u "$REDIS_URL" set marker "$MARK" >/dev/null"#;
const READ: &str = r#"printf '%s|%s|%s\n' "$(psql "$DATABASE_URL" -X -Atc "select string_agg(name, ',') from marker")" "$(redis-cli -u "$REDIS_URL" get marker)" "$(redis-cli -u "$REDIS_URL" dbsize)""#;

#[test]
fn worktrees_started_together_never_touch_each_others_services_or_data() {
    let Some(_installs) = require_installs() else {
        eprintln!("skipping: no mise on PATH with Postgres 17 and Redis 8 installed");
        return;
    };
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("stack.toml"), MANIFEST).unwrap();
    let mut fleet = Fleet { dir, worktrees: Vec::new() };
    git(&repo, &["init", "-q", "-b", "main"]);
    fleet.ok(&repo, &["compile"]);
    git(&repo, &["add", "stack.toml", "stack.lock"]);
    git(&repo, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "stack"]);
    for i in 0..WORKTREES {
        let w = repo.parent().unwrap().join(format!("wt{i}"));
        git(&repo, &["worktree", "add", "-q", w.to_str().unwrap()]);
        fleet.worktrees.push(w);
    }
    let fleet = fleet;
    fleet.each(&["compile", "--locked"]);

    // Started together, each writes its own marker using only the environment Stack gives it.
    fleet.each(&["up", "--timeout", "2m"]);
    std::thread::scope(|s| {
        for (i, w) in fleet.worktrees.iter().enumerate() {
            let fleet = &fleet;
            s.spawn(move || fleet.exec(w, &WRITE.replace("$MARK", &format!("wt{i}"))));
        }
    });
    let seen = |fleet: &Fleet| {
        for (i, w) in fleet.worktrees.iter().enumerate() {
            assert_eq!(fleet.exec(w, READ), format!("wt{i}|wt{i}|1"), "{} sees exactly its own data", w.display());
        }
    };
    seen(&fleet);

    // Distinct ports and data directories; each `status` names only its own services, verified
    // as its own instance on its own ports.
    let statuses = fleet.each(&["status"]);
    let mut ports = Vec::new();
    let mut pids = Vec::new();
    let mut data_dirs = Vec::new();
    for (w, status) in fleet.worktrees.iter().zip(&statuses) {
        assert_eq!(status["healthy"], true, "{status}");
        assert_eq!(status["session"]["project"], w.to_str().unwrap(), "{status}");
        let checks = status["checks"].as_array().unwrap();
        let names: Vec<&str> = checks.iter().map(|c| c["service"].as_str().unwrap()).collect();
        assert_eq!(names, ["postgres", "redis"], "{status}");
        let reserved = fleet.ok(w, &["inspect"])["ports"].clone();
        for c in checks {
            assert_eq!(c["identity"], "instance", "{status}");
            assert_eq!(c["port"], reserved[c["service"].as_str().unwrap()], "{status}");
            ports.push(c["port"].as_u64().unwrap());
            pids.push(c["pid"].as_u64().unwrap() as u32);
        }
        for record in status["session"]["services"].as_object().unwrap().values() {
            let data = PathBuf::from(record["data_dir"].as_str().unwrap());
            assert!(data.is_dir() && data.canonicalize().unwrap().starts_with(fleet.dir.path().canonicalize().unwrap()), "{status}");
            data_dirs.push(data);
        }
        assert_eq!(status["commands"], serde_json::json!([]), "{status}");
    }
    assert_eq!(ports.len(), 2 * WORKTREES);
    assert_eq!(ports.iter().collect::<std::collections::BTreeSet<_>>().len(), ports.len(), "a port is shared: {ports:?}");
    assert_eq!(data_dirs.len(), 2 * WORKTREES);
    assert_eq!(data_dirs.iter().collect::<std::collections::BTreeSet<_>>().len(), data_dirs.len(), "a data directory is shared: {data_dirs:?}");

    // Stopped together: confirmed, every process gone, every port free, nothing carrying a
    // worktree's token left.
    for down in fleet.each(&["down"]) {
        assert_eq!(down["confirmed"], true, "{down}");
    }
    for pid in &pids {
        assert!(stack::procinfo::process(*pid).is_none(), "service process {pid} survived down");
    }
    for port in &ports {
        assert!(!accepting(*port), "port {port} still answers after down");
    }
    for w in &fleet.worktrees {
        let token = stack::checkout::token(w).unwrap();
        assert!(stack::procinfo::carrying(stack::checkout::TOKEN_VAR, &token).is_empty(), "{} left a process", w.display());
        let status: Value = serde_json::from_slice(&fleet.stack(w, &["--json", "status"]).stdout).unwrap();
        assert!(status["data"]["session"].is_null() && status["data"]["healthy"] == false, "{status}");
    }

    // Started again, each finds its own data where it left it.
    fleet.each(&["up", "--timeout", "2m"]);
    seen(&fleet);
    for down in fleet.each(&["down"]) {
        assert_eq!(down["confirmed"], true, "{down}");
    }
    eprintln!("fleet of {WORKTREES} worktrees: {:.1}s", started.elapsed().as_secs_f64());
}

/// With real services: a command still running keeps `down --purge` unconfirmed, yet every
/// service is stopped and its data kept; a shell that runs `stack down --purge` and carries on
/// counts as such a command. Once it ends, `down --purge` confirms and removes the data.
#[test]
fn a_running_command_keeps_down_purge_from_removing_data_while_services_stop() {
    if require_installs().is_none() {
        eprintln!("skipping: no mise on PATH with Postgres 17 and Redis 8 installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().canonicalize().unwrap().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(app.join("stack.toml"), MANIFEST).unwrap();
    let mut fleet = Fleet { dir, worktrees: Vec::new() };
    git(&app, &["init", "-q", "-b", "main"]);
    fleet.ok(&app, &["compile"]);
    fleet.worktrees.push(app.clone());
    let fleet = fleet;

    let up = |fleet: &Fleet| {
        fleet.ok(&app, &["up", "--timeout", "2m"]);
        let status = fleet.ok(&app, &["status"]);
        let pids: Vec<u32> = status["checks"].as_array().unwrap().iter().map(|c| c["pid"].as_u64().unwrap() as u32).collect();
        let ports: Vec<u64> = status["checks"].as_array().unwrap().iter().map(|c| c["port"].as_u64().unwrap()).collect();
        let data: Vec<PathBuf> = status["session"]["services"].as_object().unwrap().values().map(|r| PathBuf::from(r["data_dir"].as_str().unwrap())).collect();
        assert_eq!(data.len(), 2, "{status}");
        (pids, ports, data)
    };
    let stopped_and_kept = |pids: &[u32], ports: &[u64], data: &[PathBuf]| {
        assert!(pids.iter().all(|p| stack::procinfo::process(*p).is_none()), "a service survived down");
        assert!(ports.iter().all(|p| !accepting(*p)), "a port still answers");
        assert!(data.iter().all(|d| d.is_dir()), "data was removed while a command ran: {data:?}");
    };

    // A command started with `stack exec` still running.
    let (pids, ports, data) = up(&fleet);
    let mut exec = Command::new(env!("CARGO_BIN_EXE_stack"))
        .args(["-C", app.to_str().unwrap(), "exec", "--", "/bin/sleep", "60"])
        .env("STACK_STATE_DIR", fleet.dir.path().join("state"))
        .env("STACK_CACHE_DIR", fleet.dir.path().join("cache"))
        .env_remove("STACK_COMMAND_MARK")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let sleep = loop {
        if let Some(pid) = fleet.ok(&app, &["status"])["commands"][0]["pid"].as_u64() {
            break pid as u32;
        }
        assert!(Instant::now() < deadline, "the command was never listed");
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = fleet.stack(&app, &["--json", "down", "--purge"]);
    let down: Value = serde_json::from_slice(&out.stdout).unwrap();
    // SAFETY: kill only sends a signal, to the command this test started through Stack.
    let ended = |pid: u32| unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if down["error"]["code"] != "stop_unconfirmed" {
        ended(sleep);
        let _ = exec.wait();
        panic!("{down}");
    }
    assert_eq!(down["error"]["details"][0]["pid"], sleep, "{down}");
    stopped_and_kept(&pids, &ports, &data);
    ended(sleep);
    let _ = exec.wait();

    // A shell that runs `stack down --purge` and carries on.
    let (pids, ports, data) = up(&fleet);
    let script = format!("'{}' -C '{}' --json down --purge; echo; echo carried-on", env!("CARGO_BIN_EXE_stack"), app.display());
    let out = fleet.ok(&app, &["exec", "--", "/bin/sh", "-c", &script]);
    let text = out["stdout"].as_str().unwrap();
    let inner: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(inner["error"]["code"], "stop_unconfirmed", "{text}");
    assert!(text.contains("carried-on"), "{text}");
    stopped_and_kept(&pids, &ports, &data);

    // Nothing runs: confirmed, and the data goes.
    let down = fleet.ok(&app, &["down", "--purge"]);
    assert_eq!((&down["confirmed"], &down["purge"]["complete"]), (&Value::Bool(true), &Value::Bool(true)), "{down}");
    assert!(data.iter().all(|d| !d.exists()), "{down}");
}
