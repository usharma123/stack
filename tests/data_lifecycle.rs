//! Init tasks and snapshots, against the fake mise (tests/fakes/mise.sh) with a thin wrapper
//! that runs task bodies and initialises preset data directories the way mise does, and a
//! fake `psql` that reports the data directory the supervisor started the server with (as
//! Postgres reports `-D`).
#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;
#[path = "support/servers.rs"]
mod servers;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

/// Runs task bodies (`mise run --skip-deps --no-timings <task>`) from the configuration mise
/// would load. Everything else goes to the shared fake.
const WRAPPER: &str = r#"#!/bin/sh
self=$(cd "$(dirname "$0")" && pwd)
py=${REVIEW_PYTHON:-python3}
case "$1 $2" in
  'run --skip-deps')
    task=$4
    config=$MISE_OVERRIDE_CONFIG_FILENAMES
    test -f "$config" || config=$MISE_GLOBAL_CONFIG_FILE
    body=$("$py" -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["tasks"][sys.argv[2]]["run"])' "$config" "$task") || exit 97
    exec sh -c "$body" ;;
esac
exec "$self/mise-base" "$@"
"#;


const STACK: &str = r#"[services.db]
preset = "postgres"
version = "17"
init = ["migrate", "seed"]

[tasks.migrate]
run = 'echo "$DB_DATA migrate" >> "$REVIEW_FIXTURE/tasks.log"; echo migrate >> "$DB_DATA/applied"'
services = ["db"]

[tasks.seed]
run = 'test ! -f "$REVIEW_FIXTURE/fail-seed" || exit 4; echo "$DB_DATA seed" >> "$REVIEW_FIXTURE/tasks.log"; echo seed >> "$DB_DATA/applied"'
services = ["db"]
"#;

struct Fixture {
    dir: TempDir,
    /// Each checkout's directory, by the name a test gives it (see `env_for`).
    roots: std::cell::RefCell<std::collections::HashMap<String, PathBuf>>,
}

fn write_exe(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn result(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

impl Fixture {
    /// A Git repository at `repo/` whose committed stack.toml is `stack`.
    fn new(stack: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("bin")).unwrap();
        write_exe(&root.join("bin/mise-base"), FAKE_MISE);
        write_exe(&root.join("bin/mise"), WRAPPER);
        servers::install(&root.join("bin"));
        // Knows no daemon by id, as once the fake supervisor stopped it.
        write_exe(&root.join("bin/pitchfork"), "#!/bin/sh\ncase \"$1 $2\" in 'status --json') echo 'daemon not found' >&2; exit 1 ;; esac\nexit 0\n");
        fs::write(root.join("daemons.json"), "[]").unwrap();
        fs::create_dir(root.join("repo")).unwrap();
        fs::write(root.join("repo/stack.toml"), stack).unwrap();
        git(&root.join("repo"), &["init", "-q"]);
        git(&root.join("repo"), &["add", "stack.toml"]);
        git(&root.join("repo"), &["commit", "-q", "-m", "stack"]);
        let fixture = Self { dir, roots: Default::default() };
        fixture.env_for(&fixture.repo(), None);
        fixture
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn command(&self, at: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .args(["-C", at.to_str().unwrap()])
            .args(args)
            .env("PATH", format!("{}:{}", self.dir.path().join("bin").display(), std::env::var("PATH").unwrap()))
            .env("REVIEW_FIXTURE", self.dir.path())
            .env("REVIEW_PYTHON", python::python())
            .env("STACK_STATE_DIR", self.dir.path().join("state"))
            .env("STACK_CACHE_DIR", self.dir.path().join("cache"));
        command
    }

    fn json(&self, at: &Path, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        result(&self.command(at, &args).output().unwrap())
    }

    fn ok(&self, at: &Path, args: &[&str]) -> Value {
        let v = self.json(at, args);
        assert_eq!(v["ok"], true, "{args:?}: {v}");
        v["data"].clone()
    }

    /// Where Stack keeps `db`'s data for `checkout` (see `env_for`).
    fn data_dir(&self, checkout: &str) -> PathBuf {
        let root = self.roots.borrow()[checkout].clone();
        self.dir.path().join("state/data").join(stack::state::project_key(&root)).join("db")
    }

    /// What the provider reports for `checkout`: its data directory, its port, its env.
    fn env_for(&self, at: &Path, checkout: Option<&str>) {
        let mut env = json!({ "PATH": format!("{}:{}", self.dir.path().join("bin").display(), std::env::var("PATH").unwrap()) });
        if let Some(checkout) = checkout {
            let ports = self.ok(at, &["inspect"])["ports"].clone();
            let port = ports["db"].as_u64().unwrap();
            if let Some(cache) = ports["cache"].as_u64() {
                env["REDIS_URL"] = json!(format!("redis://127.0.0.1:{cache}"));
            }
            self.roots.borrow_mut().insert(checkout.to_string(), at.canonicalize().unwrap());
            let data = self.data_dir(checkout);
            env["DATABASE_URL"] = json!(format!("postgresql://postgres@127.0.0.1:{port}/postgres"));
            env["DB_DATA"] = json!(data);
        }
        fs::write(self.dir.path().join("env.json"), env.to_string()).unwrap();
    }

    fn tasks_log(&self) -> String {
        fs::read_to_string(self.dir.path().join("tasks.log")).unwrap_or_default()
    }

    fn applied(&self, checkout: &str) -> String {
        fs::read_to_string(self.data_dir(checkout).join("applied")).unwrap_or_default()
    }
}

#[test]
fn init_runs_once_per_data_directory_and_resumes_after_a_failure() {
    let f = Fixture::new(STACK);
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));

    // A failing init task fails `up`, leaves the services running and records where it stopped.
    fs::write(f.dir.path().join("fail-seed"), "").unwrap();
    let failed = f.json(&a, &["up"]);
    let error = &failed["error"];
    assert_eq!(error["code"], "init_failed", "{failed}");
    assert!(error["hint"].as_str().unwrap().contains("resumes from 'seed'"), "{error}");
    let progress = error["details"].as_array().unwrap().last().unwrap();
    let steps: Vec<(String, String)> = progress["steps"].as_array().unwrap().iter().map(|s| (s["step"].as_str().unwrap().into(), s["status"].as_str().unwrap().into())).collect();
    assert!(steps.contains(&("verify".into(), "ok".into())) && steps.ends_with(&[("init".into(), "ok".into()), ("init".into(), "failed".into())]), "{steps:?}");
    assert!(error["details"].as_array().unwrap().iter().any(|d| d["task"] == "seed" && d["completed"] == json!(["migrate"])), "{error}");
    let status = f.ok(&a, &["status"]);
    assert_eq!((status["healthy"].clone(), status["init"]["db"].clone()), (json!(true), json!("failed")), "{status}");
    assert_eq!(f.applied("A"), "migrate\n");

    // The retry resumes from the failed task.
    fs::remove_file(f.dir.path().join("fail-seed")).unwrap();
    let up = f.ok(&a, &["up"]);
    assert_eq!(up["init"]["db"], json!({ "status": "done", "ran": ["seed"], "resumed_after": ["migrate"] }), "{up}");
    assert_eq!(f.applied("A"), "migrate\nseed\n");
    // And an initialised database is never initialised again.
    let log = f.tasks_log();
    let up = f.ok(&a, &["up"]);
    assert_eq!(up["init"]["db"]["status"], "done");
    assert!(up["init"]["db"].get("ran").is_none(), "{up}");
    assert_eq!(f.tasks_log(), log);
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "done");

    // A changed task makes init stale: reported, not re-run, until --reinit.
    fs::write(a.join("stack.toml"), STACK.replace("echo seed >>", "echo seed2 >>")).unwrap();
    f.ok(&a, &["compile"]);
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "stale");
    let up = f.ok(&a, &["up"]);
    assert_eq!(up["init"]["db"]["status"], "stale", "{up}");
    assert!(up["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("--reinit db")), "{up}");
    assert_eq!(f.applied("A"), "migrate\nseed\n");
    let up = f.ok(&a, &["up", "--reinit", "db"]);
    assert_eq!(up["init"]["db"]["ran"], json!(["migrate", "seed"]), "{up}");
    assert_eq!(f.applied("A"), "migrate\nseed\nmigrate\nseed2\n");
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "done");

    // A recreated data directory is a new database: init is pending again; --no-init skips it.
    f.ok(&a, &["down"]);
    // The init state outlives the session: a checkout not started still shows it.
    let out = f.command(&a, &["status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.starts_with("not started; run `stack up`\n") && text.contains("db           init done"), "{text}");
    fs::remove_dir_all(f.data_dir("A")).unwrap();
    f.env_for(&a, Some("A"));
    let up = f.ok(&a, &["up", "--no-init"]);
    assert_eq!(up["init"]["db"]["status"], "pending", "{up}");
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "pending");
    let up = f.ok(&a, &["up"]);
    assert_eq!(up["init"]["db"]["ran"], json!(["migrate", "seed"]), "{up}");

    // A purge removes the data, and with it the record that it was initialised.
    let down = f.ok(&a, &["down", "--purge"]);
    assert_eq!(down["purge"]["complete"], true, "{down}");
    assert!(!f.data_dir("A").exists());
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "pending");
}

#[test]
fn snapshots_seed_a_worktree_without_running_init() {
    let f = Fixture::new(STACK);
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    assert_eq!(f.ok(&a, &["up"])["init"]["db"]["ran"], json!(["migrate", "seed"]));

    // Save the migrated, seeded state; the running service is stopped for the copy and
    // started and verified again.
    let saved = f.ok(&a, &["snapshot", "save", "base"]);
    assert_eq!(saved["restarted"], json!(["db"]), "{saved}");
    assert_eq!(saved["checks"][0]["identity"], "instance", "{saved}");
    assert_eq!(saved["meta"]["services"]["db"]["init"], "done");
    assert_eq!(saved["meta"]["services"]["db"]["version"], "17.11");
    let steps: Vec<&str> = saved["steps"].as_array().unwrap().iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["stop", "copy", "start", "verify"]);
    assert_eq!(f.json(&a, &["snapshot", "save", "base"])["error"]["code"], "snapshot_exists");
    // --force replaces it whole: an exchange where the filesystem has one, never an empty name.
    let forced = f.ok(&a, &["snapshot", "save", "base", "--force"]);
    let copy = forced["steps"].as_array().unwrap().iter().find(|s| s["step"] == "copy").unwrap();
    let publish = copy["detail"]["publish"].as_str().unwrap();
    assert!(publish == "exchange" || publish == "rename", "{forced}");
    #[cfg(target_os = "macos")]
    assert_eq!(publish, "exchange", "APFS swaps directories: {forced}");
    let store: Vec<String> = fs::read_dir(f.dir.path().join("state/snapshots")).unwrap().flatten().filter(|e| e.path().is_dir()).flat_map(|e| fs::read_dir(e.path()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap())).collect();
    assert_eq!(store, ["base"], "nothing staged is left: {store:?}");
    f.ok(&a, &["down"]);

    // A new worktree of the same repository starts from it instead of running init.
    let b = f.dir.path().join("wt");
    git(&a, &["worktree", "add", "-q", b.to_str().unwrap()]);
    f.ok(&b, &["compile"]);
    let list = f.ok(&b, &["snapshot", "list"]);
    assert_eq!(list["snapshots"][0]["name"], "base", "worktrees share snapshots: {list}");
    f.env_for(&b, Some("B"));
    let missing = f.json(&b, &["up", "--from-snapshot", "nope"]);
    assert_eq!(missing["error"]["code"], "snapshot_not_found", "{missing}");
    assert!(!f.data_dir("B").exists(), "nothing started before the snapshot was checked");
    let log = f.tasks_log();
    let up = f.ok(&b, &["up", "--from-snapshot", "base"]);
    let restore = up["steps"].as_array().unwrap().iter().find(|s| s["step"] == "restore_snapshot").unwrap();
    assert_eq!(restore["detail"]["restored"][0]["service"], "db", "{up}");
    assert_eq!(up["init"]["db"]["status"], "done", "{up}");
    assert_eq!(f.tasks_log(), log, "no init task ran in the restored worktree");
    assert_eq!(f.applied("B"), "migrate\nseed\n");

    // The restored data verifies as B's own instance, and carries nothing naming A's.
    let status = f.ok(&b, &["status"]);
    assert_eq!(status["checks"][0]["identity"], "instance", "{status}");
    assert_eq!(status["session"]["services"]["db"]["data_dir"], json!(f.data_dir("B")));
    assert_eq!(status["init"]["db"], "done");
    let a_path = f.data_dir("A").to_string_lossy().into_owned();
    for entry in walk(&f.data_dir("B")) {
        let text = fs::read(&entry).unwrap();
        assert!(!String::from_utf8_lossy(&text).contains(&a_path), "{} names A's data directory", entry.display());
    }

    // Running it again on existing data keeps that data.
    let again = f.ok(&b, &["up", "--from-snapshot", "base"]);
    let restore = again["steps"].as_array().unwrap().iter().find(|s| s["step"] == "restore_snapshot").unwrap();
    assert_eq!(restore["detail"]["skipped"][0]["reason"], "data directory exists", "{again}");

    // Restoring explicitly replaces the data of a running checkout and restarts it.
    f.ok(&b, &["up", "--reinit", "db"]);
    assert_eq!(f.applied("B"), "migrate\nseed\nmigrate\nseed\n");
    let restored = f.ok(&b, &["snapshot", "restore", "base"]);
    assert_eq!(restored["restarted"], json!(["db"]), "{restored}");
    assert_eq!(f.applied("B"), "migrate\nseed\n");
    assert_eq!(f.ok(&b, &["status"])["init"]["db"], "done");
    // Its init digest is the one the snapshot satisfied: an edited task makes it stale.
    fs::write(b.join("stack.toml"), STACK.replace("echo seed >>", "echo seed2 >>")).unwrap();
    f.ok(&b, &["compile"]);
    assert_eq!(f.ok(&b, &["status"])["init"]["db"], "stale");
    // The record names the restored directory, so a purge removes it, and with it its init.
    let down = f.ok(&b, &["down", "--purge"]);
    assert_eq!(down["purge"]["complete"], true, "{down}");
    assert!(!f.data_dir("B").exists());
    assert_eq!(f.ok(&b, &["status"])["init"]["db"], "pending");
    f.ok(&b, &["snapshot", "rm", "base"]);
    assert_eq!(f.ok(&a, &["snapshot", "list"])["snapshots"], json!([]));
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn snapshots_refuse_another_major_version_and_unsupported_services() {
    let f = Fixture::new(STACK);
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    // Data Stack never recorded for this checkout is never copied.
    fs::create_dir_all(f.data_dir("A")).unwrap();
    let unrecorded = f.json(&a, &["snapshot", "save", "early", "--service", "db"]);
    assert_eq!(unrecorded["error"]["code"], "snapshot_failed", "{unrecorded}");
    fs::remove_dir_all(f.data_dir("A")).unwrap();
    // `up` records it; once stopped, the data is copied without starting anything.
    f.ok(&a, &["up", "--no-init"]);
    f.ok(&a, &["down"]);
    fs::write(a.join("stack.toml"), format!("{STACK}\n[services.web]\nrun = 'true'\n")).unwrap();
    f.ok(&a, &["compile"]);
    let saved = f.ok(&a, &["snapshot", "save", "v17", "--service", "db"]);
    assert_eq!(saved["restarted"], json!([]), "{saved}");
    assert_eq!(saved["meta"]["services"]["db"]["init"], "pending");
    assert_eq!(f.json(&a, &["snapshot", "save", "w", "--service", "web"])["error"]["code"], "snapshot_unsupported");
    assert_eq!(f.json(&a, &["snapshot", "save", "../x"])["error"]["code"], "usage");

    fs::write(a.join("stack.toml"), STACK.replace("version = \"17\"", "version = \"18.1\"")).unwrap();
    f.ok(&a, &["compile"]);
    let refused = f.json(&a, &["snapshot", "restore", "v17"]);
    assert_eq!(refused["error"]["code"], "snapshot_incompatible", "{refused}");
    assert_eq!(fs::read_to_string(f.data_dir("A").join("PG_VERSION")).unwrap(), "17\n", "nothing was replaced");
    let up = f.json(&a, &["up", "--from-snapshot", "v17"]);
    assert_eq!(up["error"]["code"], "snapshot_incompatible", "{up}");
}

#[test]
fn init_lists_are_validated_composed_and_shown_with_their_origin() {
    let f = Fixture::new("[services.db]\npreset = 'postgres'\ninit = ['nope']\n");
    let e = f.json(&f.repo(), &["inspect"]);
    assert_eq!(e["error"]["code"], "init_invalid", "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("'nope'"), "{e}");

    let excluded = "[services.db]\npreset = 'postgres'\ninit = ['m']\n[services.cache]\npreset = 'redis'\n[tasks.m]\nrun = 'true'\nservices = ['cache']\n";
    fs::write(f.repo().join("stack.toml"), excluded).unwrap();
    let e = f.json(&f.repo(), &["inspect"]);
    assert_eq!(e["error"]["code"], "init_invalid", "{e}");
    assert!(e["error"]["hint"].as_str().unwrap().contains("add \"db\" to [tasks.m] services"), "{e}");

    // A bundle declares a service with its init; the project adds nothing but an init-only
    // table for another service, and must override to change the bundle's list.
    fs::create_dir(f.dir.path().join("bundle")).unwrap();
    fs::write(
        f.dir.path().join("bundle/bundle.toml"),
        "[bundle]\nname = 'pg'\n[services.db]\npreset = 'postgres'\nversion = '17'\ninit = ['migrate']\n[services.cache]\npreset = 'redis'\n[tasks.migrate]\nrun = 'true'\nservices = ['db']\n[tasks.warm]\nrun = 'true'\n",
    )
    .unwrap();
    let base = "[[use]]\nbundle = 'path:../bundle'\n[services.cache]\ninit = ['warm']\n";
    fs::write(f.repo().join("stack.toml"), base).unwrap();
    let inspect = f.ok(&f.repo(), &["inspect"]);
    assert_eq!(inspect["init"]["db"], json!({ "value": ["migrate"], "origin": "bundle:pg" }), "{inspect}");
    assert_eq!(inspect["init"]["cache"], json!({ "value": ["warm"], "origin": "project" }));
    assert!(inspect["stack"]["services"]["cache"]["value"].get("init").is_none(), "init stays out of the service definition");

    fs::write(f.repo().join("stack.toml"), format!("{base}[services.db]\ninit = ['migrate', 'warm']\n")).unwrap();
    let e = f.json(&f.repo(), &["inspect"]);
    assert_eq!(e["error"]["code"], "conflict", "{e}");
    assert!(e["error"]["hint"].as_str().unwrap().contains("[override.services.db] init"), "{e}");
    fs::write(f.repo().join("stack.toml"), format!("{base}[override.services.db]\ninit = ['migrate', 'warm']\n")).unwrap();
    let inspect = f.ok(&f.repo(), &["inspect"]);
    assert_eq!(inspect["init"]["db"], json!({ "value": ["migrate", "warm"], "origin": "override" }), "{inspect}");
    assert_eq!(inspect["stack"]["services"]["db"]["value"]["preset"], "postgres", "an init-only override keeps the definition");

    fs::write(f.repo().join("stack.toml"), "[services.ghost]\ninit = ['x']\n[tasks.x]\nrun = 'true'\n").unwrap();
    assert_eq!(f.json(&f.repo(), &["inspect"])["error"]["code"], "init_invalid");
}

#[test]
fn a_service_without_a_data_directory_runs_init_once_per_checkout() {
    let listen = r#"exec "$REVIEW_PYTHON" -c 'import os,socket,time; s=socket.socket(); s.bind(("127.0.0.1", int(os.environ["PORT"]))); s.listen(); time.sleep(600)'"#;
    let f = Fixture::new(&format!("[services.web]\nrun = '''{listen}'''\ninit = ['setup']\n[tasks.setup]\nrun = 'echo setup >> \"$REVIEW_FIXTURE/tasks.log\"'\n"));
    let a = f.repo();
    f.ok(&a, &["compile"]);
    assert_eq!(f.ok(&a, &["up"])["init"]["web"]["ran"], json!(["setup"]));
    assert_eq!(f.ok(&a, &["up"])["init"]["web"]["status"], "done");
    assert_eq!(f.tasks_log(), "setup\n");
    let e = f.json(&a, &["up", "--reinit", "nope"]);
    assert_eq!(e["error"]["code"], "unknown_service", "{e}");
    let e = f.json(&a, &["up", "--no-init", "--reinit", "web"]);
    assert_eq!(e["error"]["code"], "usage", "{e}");
    assert_eq!(f.ok(&a, &["up", "--reinit", "web"])["init"]["web"]["ran"], json!(["setup"]));
    assert_eq!(f.tasks_log(), "setup\nsetup\n");
    f.ok(&a, &["down"]);
}

#[test]
fn mcp_exposes_snapshots_and_init_options() {
    let f = Fixture::new(STACK);
    let repo = f.repo();
    let calls = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stack_snapshot_list","arguments":{"dir":repo}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"stack_up","arguments":{"dir":repo,"no_init":true,"reinit":["db"]}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"stack_snapshot_restore","arguments":{"dir":repo,"name":"none"}}}),
    ];
    let input: String = calls.iter().map(|c| format!("{c}\n")).collect();
    let mut child = f.command(&repo, &["mcp"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let replies: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap()["result"].clone();
    let names: Vec<String> = by_id(1)["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    for tool in ["stack_snapshot_save", "stack_snapshot_restore", "stack_snapshot_list", "stack_snapshot_rm"] {
        assert!(names.iter().any(|n| n == tool), "{tool}: {names:?}");
    }
    assert_eq!(by_id(2)["structuredContent"]["data"]["snapshots"], json!([]));
    assert_eq!(by_id(2)["structuredContent"]["data"]["project"]["kind"], "git");
    assert_eq!(by_id(3)["structuredContent"]["error"]["code"], "usage");
    assert_eq!(by_id(4)["structuredContent"]["error"]["code"], "snapshot_not_found");
}

/// `STACK` with a Redis service beside Postgres: two snapshotted services.
fn two_services() -> String {
    format!("{STACK}\n[services.cache]\npreset = \"redis\"\nversion = \"8\"\n")
}

impl Fixture {
    /// Where Stack keeps `service`'s data for `checkout`.
    fn data_of(&self, checkout: &str, service: &str) -> PathBuf {
        self.data_dir(checkout).with_file_name(service)
    }

    /// The restore journal of `checkout` (see `snapshot::restore_services`).
    fn journal(&self, checkout: &str) -> PathBuf {
        let root = self.roots.borrow()[checkout].clone();
        self.dir.path().join("state/init").join(format!("{}.restore.json", stack::state::project_key(&root)))
    }

    fn snapshot_dir(&self, name: &str) -> PathBuf {
        let store = self.dir.path().join("state/snapshots");
        fs::read_dir(&store).unwrap().flatten().map(|e| e.path()).find(|p| p.is_dir()).unwrap().join(name)
    }
}

fn ino(path: &Path) -> [u64; 2] {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path).unwrap();
    [m.dev(), m.ino()]
}

#[test]
fn restored_data_resumes_init_where_the_snapshot_stopped() {
    let f = Fixture::new(STACK);
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    fs::write(f.dir.path().join("fail-seed"), "").unwrap();
    assert_eq!(f.json(&a, &["up"])["error"]["code"], "init_failed");
    assert_eq!(f.applied("A"), "migrate\n");
    // Saved with init failed after `migrate`: the snapshot records where it stopped.
    let saved = f.ok(&a, &["snapshot", "save", "half"]);
    let db = &saved["meta"]["services"]["db"];
    assert_eq!(db["init"], "failed", "{saved}");
    assert!(db.get("init_digest").is_none(), "never counted as done: {saved}");
    assert_eq!(db["init_resume"]["completed"][0]["task"], "migrate", "{saved}");
    assert_eq!(db["init_resume"]["failed_task"], "seed", "{saved}");
    f.ok(&a, &["down"]);
    fs::remove_file(f.dir.path().join("fail-seed")).unwrap();

    // Restored, `up` resumes from the failed task, exactly as the source checkout would.
    let b = f.dir.path().join("wt");
    git(&a, &["worktree", "add", "-q", b.to_str().unwrap()]);
    f.ok(&b, &["compile"]);
    f.env_for(&b, Some("B"));
    let log = f.tasks_log();
    let up = f.ok(&b, &["up", "--from-snapshot", "half"]);
    assert_eq!(up["init"]["db"], json!({ "status": "done", "ran": ["seed"], "resumed_after": ["migrate"] }), "{up}");
    assert_eq!(f.applied("B"), "migrate\nseed\n");
    assert_eq!(f.tasks_log(), format!("{log}{} seed\n", f.data_dir("B").display()), "only the failed task ran, once");
    // Restored again over it: the same replay, from the same point.
    let restored = f.ok(&b, &["snapshot", "restore", "half"]);
    assert_eq!(restored["restarted"], json!(["db"]), "{restored}");
    assert_eq!(f.ok(&b, &["status"])["init"]["db"], "failed");
    let up = f.ok(&b, &["up"]);
    assert_eq!(up["init"]["db"]["ran"], json!(["seed"]), "{up}");
    f.ok(&b, &["down"]);
}

#[test]
fn a_restore_of_several_services_changes_all_or_none() {
    let f = Fixture::new(&two_services());
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    f.ok(&a, &["up"]);
    let saved = f.ok(&a, &["snapshot", "save", "both"]);
    assert_eq!(saved["meta"]["services"].as_object().unwrap().keys().collect::<Vec<_>>(), ["db", "cache"], "{saved}");
    f.ok(&a, &["snapshot", "save", "spare"]);
    for service in ["db", "cache"] {
        fs::write(f.data_of("A", service).join("since"), "written after the snapshot").unwrap();
    }
    let before = [ino(&f.data_of("A", "db")), ino(&f.data_of("A", "cache"))];

    // The second service's copy fails: the first is not replaced either.
    fs::remove_dir_all(f.snapshot_dir("both").join("data/cache")).unwrap();
    let failed = f.json(&a, &["snapshot", "restore", "both"]);
    let error = &failed["error"];
    assert_eq!(error["code"], "snapshot_failed", "{failed}");
    let services = error["details"].as_array().unwrap().iter().find_map(|d| d["services"].as_array()).unwrap();
    let outcomes: Vec<(&str, &str)> = services.iter().map(|s| (s["service"].as_str().unwrap(), s["outcome"].as_str().unwrap())).collect();
    assert_eq!(outcomes, [("db", "unchanged"), ("cache", "unchanged")], "{failed}");
    assert_eq!([ino(&f.data_of("A", "db")), ino(&f.data_of("A", "cache"))], before);
    assert!(f.data_of("A", "db").join("since").exists() && f.data_of("A", "cache").join("since").exists());
    let status = f.ok(&a, &["status"]);
    assert_eq!(status["healthy"], true, "the services were started again on their data: {status}");
    assert!(!f.journal("A").exists());

    // A whole snapshot replaces both, and the init state of the restored data is recorded.
    let restored = f.ok(&a, &["snapshot", "restore", "spare"]);
    let mut restarted: Vec<&str> = restored["restarted"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    restarted.sort();
    assert_eq!(restarted, ["cache", "db"], "{restored}");
    assert!(!f.data_of("A", "db").join("since").exists() && !f.data_of("A", "cache").join("since").exists());
    assert_eq!(f.applied("A"), "migrate\nseed\n");
    assert_eq!(f.ok(&a, &["status"])["init"]["db"], "done");
    let parent: Vec<String> = fs::read_dir(f.data_dir("A").parent().unwrap()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).filter(|n| n.contains(".stack-restore") || n.contains(".stack-old")).collect();
    assert!(parent.is_empty(), "no copy or old data is left: {parent:?}");
    f.ok(&a, &["down"]);
}

#[test]
fn up_finishes_or_refuses_a_restore_that_was_interrupted() {
    let f = Fixture::new(&two_services());
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    f.ok(&a, &["up"]);
    f.ok(&a, &["down"]);
    let log = f.tasks_log();

    // As a crash leaves it: db exchanged with its restored copy, the journal not committed.
    let data = f.data_dir("A");
    let target = data.canonicalize().unwrap();
    let old = ino(&target);
    let fresh = target.with_file_name(".db.stack-restore-99-1");
    let copy = target.with_file_name("copy");
    fs::create_dir(&copy).unwrap();
    fs::write(copy.join("applied"), "restored\n").unwrap();
    fs::rename(&target, &fresh).unwrap();
    fs::rename(&copy, &target).unwrap();
    let journal = json!({
        "snapshot": "x", "phase": "swapping", "token": "t", "markers": { "db": null },
        "swaps": [{ "service": "db", "dir": data, "target": target, "fresh": fresh, "aside": target.with_file_name(".db.stack-old-99-1"), "old": old, "new": ino(&target) }],
    });
    fs::create_dir_all(f.journal("A").parent().unwrap()).unwrap();
    fs::write(f.journal("A"), journal.to_string()).unwrap();

    let up = f.ok(&a, &["up"]);
    let step = up["steps"].as_array().unwrap().iter().find(|s| s["step"] == "recover_restore").unwrap_or_else(|| panic!("{up}"));
    assert_eq!(step["detail"]["interrupted_restore"], "rolled_back", "{up}");
    assert_eq!(ino(&target), old, "the very directory that was there is back");
    assert_eq!(f.applied("A"), "migrate\nseed\n");
    assert_eq!(up["init"]["db"]["status"], "done", "{up}");
    assert_eq!(f.tasks_log(), log, "no init task ran: the data is the one init ran against");
    assert!(!fresh.exists() && !f.journal("A").exists());

    // One that cannot be undone (the old data is nowhere to be found): nothing starts on it,
    // and nothing copies it.
    f.ok(&a, &["up"]);
    f.ok(&a, &["snapshot", "save", "now"]);
    let journal = json!({
        "snapshot": "x", "phase": "swapping", "token": "t", "markers": {},
        "swaps": [{ "service": "db", "dir": data, "target": target, "fresh": fresh, "aside": target.with_file_name(".db.stack-old-99-1"), "old": [1, 2], "new": [3, 4] }],
    });
    fs::write(f.journal("A"), journal.to_string()).unwrap();
    let restore = f.json(&a, &["snapshot", "restore", "x"]);
    assert_eq!(restore["error"]["code"], "snapshot_not_found", "checked before anything stops: {restore}");
    let restore = f.json(&a, &["snapshot", "restore", "now"]);
    assert_eq!(restore["error"]["code"], "restore_incomplete", "{restore}");
    let services = restore["error"]["details"].as_array().unwrap().iter().find_map(|d| d["services"].as_array()).unwrap();
    assert_eq!(services[0]["outcome"], "manual_recovery", "{restore}");
    let progress = restore["error"]["details"].as_array().unwrap().iter().find(|d| d.get("left_stopped").is_some()).unwrap_or_else(|| panic!("{restore}"));
    let mut left: Vec<&str> = progress["left_stopped"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    left.sort();
    assert_eq!(left, ["cache", "db"], "{restore}");
    let status = f.json(&a, &["status"]);
    assert_ne!(status["data"]["healthy"], true, "nothing was started on that data: {status}");
    assert_eq!(f.json(&a, &["snapshot", "save", "later"])["error"]["code"], "restore_incomplete");
    let up = f.json(&a, &["up"]);
    assert_eq!(up["error"]["code"], "restore_incomplete", "{up}");
    // Resolved by hand, as the hint says: the journal removed, `up` starts again.
    fs::remove_file(f.journal("A")).unwrap();
    f.ok(&a, &["up"]);
    f.ok(&a, &["down"]);
}

#[test]
fn an_interrupted_restore_is_finished_only_with_every_service_it_covers_stopped() {
    let f = Fixture::new(&two_services());
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    f.ok(&a, &["up"]);
    // As a crash leaves it before any swap: cache's restored copy made, nothing replaced yet.
    let cache = f.data_of("A", "cache").canonicalize().unwrap();
    let before = ino(&cache);
    let fresh = cache.with_file_name(".cache.stack-restore-99-1");
    fs::create_dir(&fresh).unwrap();
    let journal = |service: &str, target: &Path| {
        json!({
            "snapshot": "x", "phase": "swapping", "token": "t", "markers": {},
            "swaps": [{ "service": service, "dir": f.data_of("A", "cache"), "target": target, "fresh": fresh, "aside": target.with_file_name(".cache.stack-old-99-1"), "old": ino(target), "new": ino(&fresh) }],
        })
        .to_string()
    };
    fs::create_dir_all(f.journal("A").parent().unwrap()).unwrap();
    fs::write(f.journal("A"), journal("cache", &cache)).unwrap();

    // `restart` neither starts services on that data nor rebinds it; it changes nothing.
    let restart = f.json(&a, &["restart", "db"]);
    assert_eq!(restart["error"]["code"], "restore_incomplete", "{restart}");
    assert!(f.journal("A").exists() && fresh.exists());
    assert_eq!(f.ok(&a, &["status"])["healthy"], true, "nothing was stopped");

    // A save of db alone also stops cache, which the restore covers, before finishing it, and
    // starts both again after.
    let saved = f.ok(&a, &["snapshot", "save", "db-only", "--service", "db"]);
    let mut restarted: Vec<&str> = saved["restarted"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    restarted.sort();
    assert_eq!(restarted, ["cache", "db"], "{saved}");
    let copy = saved["steps"].as_array().unwrap().iter().find(|s| s["step"] == "copy").unwrap_or_else(|| panic!("{saved}"));
    assert_eq!(copy["detail"]["interrupted_restore"]["interrupted_restore"], "rolled_back", "{saved}");
    assert_eq!(ino(&cache), before);
    assert!(!fresh.exists() && !f.journal("A").exists());
    assert_eq!(f.ok(&a, &["status"])["healthy"], true);
    f.ok(&a, &["restart", "db"]);
    f.ok(&a, &["down"]);

    // A journaled service stack.toml no longer defines is checked all the same: not running, so
    // the restore is finished.
    fs::write(a.join("stack.toml"), STACK).unwrap();
    git(&a, &["commit", "-qam", "drop cache"]);
    f.ok(&a, &["compile"]);
    fs::create_dir(&fresh).unwrap();
    fs::write(f.journal("A"), journal("cache", &cache)).unwrap();
    let saved = f.ok(&a, &["snapshot", "save", "after", "--service", "db"]);
    let copy = saved["steps"].as_array().unwrap().iter().find(|s| s["step"] == "copy").unwrap_or_else(|| panic!("{saved}"));
    assert_eq!(copy["detail"]["interrupted_restore"]["interrupted_restore"], "rolled_back", "{saved}");
    assert!(!fresh.exists() && !f.journal("A").exists());
}

#[test]
fn a_committed_restore_whose_copy_vanished_is_never_recorded() {
    let f = Fixture::new(STACK);
    let a = f.repo();
    f.ok(&a, &["compile"]);
    f.env_for(&a, Some("A"));
    f.ok(&a, &["up"]);
    f.ok(&a, &["down"]);
    let markers = || fs::read_to_string(f.dir.path().join("state/init").join(format!("{}.json", stack::state::project_key(&f.roots.borrow()["A"])))).unwrap_or_default();
    let ledger = || fs::read_to_string(f.dir.path().join("state/data.json")).unwrap();
    let (markers_before, ledger_before) = (markers(), ledger());
    // As a committed restore whose recording was interrupted leaves it, but the restored copy
    // is no longer at db's place: what is there is the data that was there before the restore.
    let data = f.data_dir("A");
    let target = data.canonicalize().unwrap();
    let aside = target.with_file_name(".db.stack-old-99-1");
    fs::create_dir(&aside).unwrap();
    fs::write(aside.join("applied"), "old data\n").unwrap();
    let token = stack::checkout::token(&f.roots.borrow()["A"]).unwrap();
    let journal = json!({
        "snapshot": "x", "phase": "committed", "token": token, "markers": { "db": null },
        "swaps": [{ "service": "db", "dir": data, "target": target, "fresh": target.with_file_name(".db.stack-restore-99-1"), "aside": aside, "old": ino(&aside), "new": [1, 2] }],
    });
    fs::create_dir_all(f.journal("A").parent().unwrap()).unwrap();
    fs::write(f.journal("A"), journal.to_string()).unwrap();

    for args in [&["up"][..], &["snapshot", "save", "y"][..]] {
        let out = f.json(&a, args);
        assert_eq!(out["error"]["code"], "restore_incomplete", "{args:?}: {out}");
        assert!(out["error"]["message"].as_str().unwrap().contains("is no longer the restored copy"), "{out}");
        let services = out["error"]["details"].as_array().unwrap().iter().find_map(|d| d["services"].as_array()).unwrap();
        assert_eq!(services[0]["outcome"], "manual_recovery", "{out}");
        assert_eq!(services[0]["old_data"], json!(aside), "{out}");
        // Nothing recorded, rebound or removed.
        assert!(f.journal("A").exists() && aside.join("applied").exists(), "{args:?}");
        assert_eq!((markers(), ledger()), (markers_before.clone(), ledger_before.clone()), "{args:?}");
    }
    // Resolved by hand, as the hint says.
    fs::remove_file(f.journal("A")).unwrap();
    fs::remove_dir_all(&aside).unwrap();
    f.ok(&a, &["up"]);
    f.ok(&a, &["down"]);
}
