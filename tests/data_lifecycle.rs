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

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

/// Runs task bodies (`mise run --skip-deps --no-timings <task>`) from the configuration mise
/// would load, and before `daemons start` creates each preset data directory that does not
/// exist yet, writing the run file a real server leaves (`postmaster.opts`, naming its own
/// path). Everything else goes to the shared fake.
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
  'daemons start')
    "$py" -c '
import json, os, sys
for d in json.load(open(sys.argv[1])):
    data = d.get("data_dir")
    if not data:
        continue
    if not os.path.isdir(data):
        os.makedirs(data, mode=0o700)
        open(os.path.join(data, ".mise-daemon-version"), "w").write("postgres:17")
        open(os.path.join(data, "PG_VERSION"), "w").write("17\n")
    open(os.path.join(data, "postmaster.opts"), "w").write("postgres \"-D\" \"%s\"\n" % data)
' "$REVIEW_FIXTURE/daemons-started.json" ;;
esac
exec "$self/mise-base" "$@"
"#;

/// `psql <url> -Atc ...`: the data directory of the running daemon on the URL's port.
const PSQL: &str = r#"#!/bin/sh
exec "${REVIEW_PYTHON:-python3}" -c '
import json, re, sys
port = int(re.search(r":(\d+)/", sys.argv[1]).group(1))
for d in json.load(open(sys.argv[2])):
    if d.get("port") == port and d.get("status") == "running":
        print(d["data_dir"]); sys.exit(0)
sys.exit("psql: connection refused")
' "$1" "$REVIEW_FIXTURE/daemons.json"
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
        write_exe(&root.join("bin/psql"), PSQL);
        // Knows no daemon by id, as once the fake supervisor stopped it.
        write_exe(&root.join("bin/pitchfork"), "#!/bin/sh\ncase \"$1 $2\" in 'status --json') echo 'daemon not found' >&2; exit 1 ;; esac\nexit 0\n");
        fs::write(root.join("daemons.json"), "[]").unwrap();
        fs::create_dir(root.join("repo")).unwrap();
        fs::write(root.join("repo/stack.toml"), stack).unwrap();
        git(&root.join("repo"), &["init", "-q"]);
        git(&root.join("repo"), &["add", "stack.toml"]);
        git(&root.join("repo"), &["commit", "-q", "-m", "stack"]);
        let fixture = Self { dir };
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

    fn data_dir(&self, checkout: &str) -> PathBuf {
        self.dir.path().join("mise-state").join(checkout).join("data/db")
    }

    /// What the provider reports for `checkout`: its data directory, its port, its env.
    fn env_for(&self, at: &Path, checkout: Option<&str>) {
        let mut env = json!({ "PATH": format!("{}:{}", self.dir.path().join("bin").display(), std::env::var("PATH").unwrap()) });
        if let Some(checkout) = checkout {
            let port = self.ok(at, &["inspect"])["ports"]["db"].as_u64().unwrap();
            let data = self.data_dir(checkout);
            let entry = |status: &str| json!([{ "id": format!("{checkout}/db"), "name": "db", "status": status, "port": port, "data_dir": data }]);
            fs::write(self.dir.path().join("daemons.json"), entry("available").to_string()).unwrap();
            fs::write(self.dir.path().join("daemons-started.json"), entry("running").to_string()).unwrap();
            fs::write(self.dir.path().join("listen-port"), port.to_string()).unwrap();
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
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
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
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
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
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
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
#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), ignore = "services need Pitchfork, which publishes no Intel macOS build")]
fn a_service_without_a_data_directory_runs_init_once_per_checkout() {
    let f = Fixture::new("[services.web]\nrun = 'true'\ninit = ['setup']\n[tasks.setup]\nrun = 'echo setup >> \"$REVIEW_FIXTURE/tasks.log\"'\n");
    let a = f.repo();
    f.ok(&a, &["compile"]);
    let port = f.ok(&a, &["inspect"])["ports"]["web"].as_u64().unwrap();
    let entry = |status: &str| json!([{ "id": "A/web", "name": "web", "status": status, "port": port }]).to_string();
    fs::write(f.dir.path().join("daemons.json"), entry("available")).unwrap();
    fs::write(f.dir.path().join("daemons-started.json"), entry("running")).unwrap();
    fs::write(f.dir.path().join("listen-port"), port.to_string()).unwrap();
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
